#!/usr/bin/env swift
import AVFoundation
import AppKit
import CoreImage
import Foundation

let outPath = CommandLine.arguments.count > 1
    ? CommandLine.arguments[1]
    : "mk3-cam.jpg"
let warmupMs = CommandLine.arguments.count > 2
    ? Int(CommandLine.arguments[2]) ?? 1500
    : 1500
let deviceHint = CommandLine.arguments.count > 3
    ? CommandLine.arguments[3]
    : ""

final class Grab: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate {
    let outURL: URL
    let skip: Int
    let session = AVCaptureSession()
    let output = AVCaptureVideoDataOutput()
    let q = DispatchQueue(label: "grab")
    var n = 0
    var done = false
    var blurSkip = 0
    var restoreCenterStage: Bool?
    let lock = NSLock()

    init(outURL: URL, warmupMs: Int) {
        self.outURL = outURL
        self.skip = max(1, warmupMs * 30 / 1000)
        super.init()
    }

    func restore() {
        if #available(macOS 12.3, *), let prev = restoreCenterStage {
            AVCaptureDevice.isCenterStageEnabled = prev
            AVCaptureDevice.centerStageControlMode = .user
        }
        session.stopRunning()
    }

    func start() throws {
        let types: [AVCaptureDevice.DeviceType] = [
            .continuityCamera,
            .builtInWideAngleCamera,
            .external,
        ]
        let discovery = AVCaptureDevice.DiscoverySession(
            deviceTypes: types,
            mediaType: .video,
            position: .unspecified
        )
        let device: AVCaptureDevice? = {
            if !deviceHint.isEmpty {
                return discovery.devices.first(where: { $0.localizedName == deviceHint })
                    ?? discovery.devices.first(where: {
                        $0.localizedName.localizedCaseInsensitiveContains(deviceHint)
                    })
            }
            return discovery.devices.first(where: {
                $0.deviceType == .continuityCamera && !$0.localizedName.contains("Desk")
            }) ?? discovery.devices.first
        }()
        guard let device else {
            throw NSError(
                domain: "grab",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey: "no camera matching \(deviceHint)"]
            )
        }
        fputs("using \(device.localizedName)\n", stderr)
        if #available(macOS 12.3, *) {
            restoreCenterStage = AVCaptureDevice.isCenterStageEnabled
            AVCaptureDevice.centerStageControlMode = .app
            AVCaptureDevice.isCenterStageEnabled = false
        }
        let input = try AVCaptureDeviceInput(device: device)
        session.beginConfiguration()
        session.sessionPreset = .high
        session.addInput(input)
        output.alwaysDiscardsLateVideoFrames = true
        output.videoSettings = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA]
        output.setSampleBufferDelegate(self, queue: q)
        session.addOutput(output)
        session.commitConfiguration()
        session.startRunning()
    }

    func captureOutput(
        _ output: AVCaptureOutput,
        didOutput sampleBuffer: CMSampleBuffer,
        from connection: AVCaptureConnection
    ) {
        lock.lock()
        defer { lock.unlock() }
        if done { return }
        n += 1
        if n < skip { return }
        guard let pb = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        CVPixelBufferLockBaseAddress(pb, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pb, .readOnly) }
        let w = CVPixelBufferGetWidth(pb)
        let h = CVPixelBufferGetHeight(pb)
        if let base = CVPixelBufferGetBaseAddress(pb) {
            let stride = CVPixelBufferGetBytesPerRow(pb)
            let ptr = base.assumingMemoryBound(to: UInt8.self)
            var acc = 0
            var samples = 0
            var edges = 0
            var y = 0
            while y + 1 < h {
                var x = 0
                while x + 1 < w {
                    let o = y * stride + x * 4
                    let b = Int(ptr[o]), g = Int(ptr[o + 1]), r = Int(ptr[o + 2])
                    let luma = (r * 3 + g * 6 + b) / 10
                    acc += luma
                    samples += 1
                    let oE = o + 4
                    let oS = o + stride
                    let lE = (Int(ptr[oE + 2]) * 3 + Int(ptr[oE + 1]) * 6 + Int(ptr[oE])) / 10
                    let lS = (Int(ptr[oS + 2]) * 3 + Int(ptr[oS + 1]) * 6 + Int(ptr[oS])) / 10
                    if abs(luma - lE) + abs(luma - lS) > 24 {
                        edges += 1
                    }
                    x += 32
                }
                y += 32
            }
            let mean = samples == 0 ? 0 : acc / samples
            if mean < 12 { return }
            // Continuity AF hunts; first non-dark frame after warmup is often soup.
            if edges < 8 {
                blurSkip += 1
                if blurSkip <= 2 || blurSkip % 30 == 0 {
                    fputs("blur skip mean=\(mean) edges=\(edges) n=\(n)\n", stderr)
                }
                return
            }
        }
        let ci = CIImage(cvPixelBuffer: pb)
        let ctx = CIContext(options: [.useSoftwareRenderer: false])
        guard let cg = ctx.createCGImage(ci, from: ci.extent) else { return }
        let rep = NSBitmapImageRep(cgImage: cg)
        guard let data = rep.representation(using: .jpeg, properties: [.compressionFactor: 0.85])
        else { return }
        if let existing = try? Data(contentsOf: outURL),
           existing.count >= 180_000,
           data.count < 180_000
        {
            fputs(
                "gate refused overwrite existing=\(existing.count) new=\(data.count)\n",
                stderr
            )
            done = true
            restore()
            DispatchQueue.main.async { CFRunLoopStop(CFRunLoopGetMain()) }
            return
        }
        try? data.write(to: outURL)
        fputs(
            "wrote \(outURL.path) \(data.count) bytes \(w)x\(h) after \(n) frames (\(blurSkip) blur)\n",
            stderr
        )
        done = true
        restore()
        DispatchQueue.main.async { CFRunLoopStop(CFRunLoopGetMain()) }
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let grab = Grab(outURL: URL(fileURLWithPath: outPath), warmupMs: warmupMs)
do {
    try grab.start()
} catch {
    fputs("start failed: \(error)\n", stderr)
    exit(1)
}
DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(max(warmupMs + 8000, 12000))) {
    fputs("timeout waiting for frame\n", stderr)
    grab.restore()
    DispatchQueue.main.async { CFRunLoopStop(CFRunLoopGetMain()) }
}
CFRunLoopRun()
exit(grab.done ? 0 : 2)
