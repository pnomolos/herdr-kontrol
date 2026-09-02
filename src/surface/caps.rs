//! Hardware is a concrete caps record (not a negotiated wire) so a later pad
//! can share glance+focus without inheriting MK3 LCD policy.
//! Device adapters ([`Surface`]) consume occupancy; herdr is the only runtime
//! for now.

use anyhow::Result;

use super::occupancy::{Focus, OccupancySource, Occupant};
use crate::device::{DISPLAY_H, DISPLAY_W};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Grid {
    pub cols: u8,
    pub rows: u8,
}

impl Grid {
    pub fn cells(self) -> usize {
        self.cols as usize * self.rows as usize
    }
}

/// NI pads/groups: `(hue << 2) | (bright & 0x3)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lights {
    IndexedHueBright,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    Rgb565,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Display {
    pub width: u16,
    pub height: u16,
    pub format: PixelFormat,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurfaceCaps {
    pub grid: Grid,
    pub lights: Lights,
    pub displays: Vec<Display>,
    pub encoders: u8,
    /// Filled after exclusive USB claim; not a negotiated feature.
    pub exclusive_usb: Option<bool>,
    pub update_interval_us: u32,
}

impl SurfaceCaps {
    pub fn with_exclusive_usb(mut self, claimed: bool) -> Self {
        self.exclusive_usb = Some(claimed);
        self
    }
}

/// First device adapter. Caps are known MK3 numbers, not a probe.
pub struct MaschineCaps;

const MK3_UPDATE_INTERVAL_US: u32 = 200_000;

impl MaschineCaps {
    pub fn mk3() -> SurfaceCaps {
        let display = Display {
            width: DISPLAY_W as u16,
            height: DISPLAY_H as u16,
            format: PixelFormat::Rgb565,
        };
        // Hardware is 4×4; host glance bank is 8 (bottom two pad rows).
        SurfaceCaps {
            grid: Grid { cols: 4, rows: 4 },
            lights: Lights::IndexedHueBright,
            displays: vec![display, display],
            encoders: 9,
            exclusive_usb: None,
            update_interval_us: MK3_UPDATE_INTERVAL_US,
        }
    }
}

/// Device edge: occupancy in, hardware out, using this surface's caps.
pub trait Surface {
    fn caps(&self) -> &SurfaceCaps;
    fn apply(&mut self, occupants: &[Occupant]) -> Result<()>;
}

pub fn paint(surface: &mut impl Surface, source: &impl OccupancySource) -> Result<()> {
    surface.apply(&source.visible(surface.caps().grid.cells()))
}

/// Glance layout from caps + occupants. Host paging stays out of the raster.
/// Cell index is linear `0..caps.grid.cells()` (MK3 physical pad 0 = bottom-left, not HID 0).
pub struct Renderer<'a> {
    caps: &'a SurfaceCaps,
}

impl<'a> Renderer<'a> {
    pub fn new(caps: &'a SurfaceCaps) -> Self {
        Self { caps }
    }

    fn occupant_at<'b>(&self, occupants: &'b [Occupant], cell: usize) -> Option<&'b Occupant> {
        (cell < self.caps.grid.cells())
            .then(|| occupants.get(cell))
            .flatten()
    }

    pub fn grid<'b>(&self, occupants: &'b [Occupant]) -> Vec<Option<&'b Occupant>> {
        (0..self.caps.grid.cells())
            .map(|i| self.occupant_at(occupants, i))
            .collect()
    }

    pub fn focus_at(&self, occupants: &[Occupant], cell: usize) -> Option<Focus> {
        self.occupant_at(occupants, cell).map(Focus::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::{AgentInfo, AgentStatus};
    use crate::surface::attention::{AttentionModel, PAGE};
    use crate::surface::occupancy::{Occupancy, OccupancySource};

    fn agent(id: &str, status: AgentStatus) -> AgentInfo {
        AgentInfo {
            terminal_id: id.into(),
            name: Some(id.into()),
            agent: Some("claude".into()),
            title: None,
            display_agent: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: status,
            state_labels: Default::default(),
            tokens: Default::default(),
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            pane_id: id.into(),
            focused: false,
            state_change_seq: 1,
            cwd: None,
            foreground_cwd: None,
            revision: 1,
        }
    }

    struct RecordingSurface {
        caps: SurfaceCaps,
        last: Vec<Occupant>,
    }

    impl Surface for RecordingSurface {
        fn caps(&self) -> &SurfaceCaps {
            &self.caps
        }

        fn apply(&mut self, occupants: &[Occupant]) -> Result<()> {
            self.last = occupants.to_vec();
            Ok(())
        }
    }

    #[test]
    fn mk3_caps() {
        let caps = MaschineCaps::mk3();
        assert_eq!(caps.grid, Grid { cols: 4, rows: 4 });
        assert_eq!(caps.grid.cells(), 16);
        assert_eq!(caps.lights, Lights::IndexedHueBright);
        assert_eq!(caps.encoders, 9);
        assert_eq!(caps.displays.len(), 2);
        for d in &caps.displays {
            assert_eq!(d.width, 480);
            assert_eq!(d.height, 272);
            assert_eq!(d.format, PixelFormat::Rgb565);
        }
        assert_eq!(caps.exclusive_usb, None);
        assert_eq!(caps.update_interval_us, MK3_UPDATE_INTERVAL_US);
        assert_eq!(
            caps.clone().with_exclusive_usb(true).exclusive_usb,
            Some(true)
        );
    }

    #[test]
    fn renderer_and_paint_use_caps() {
        let caps = MaschineCaps::mk3();
        let occupants = [
            Occupant {
                id: "a".into(),
                occupancy: Occupancy::Blocked,
            },
            Occupant {
                id: "b".into(),
                occupancy: Occupancy::Working,
            },
        ];
        let r = Renderer::new(&caps);
        let grid = r.grid(&occupants);
        assert_eq!(grid.len(), 16);
        assert_eq!(grid[0].map(|o| o.occupancy), Some(Occupancy::Blocked));
        assert_eq!(grid[0].map(|o| o.id.as_str()), Some("a"));
        assert_eq!(grid[1].map(|o| o.occupancy), Some(Occupancy::Working));
        assert!(grid[2..].iter().all(|s| s.is_none()));
        assert_eq!(
            r.focus_at(&occupants, 1),
            Some(Focus {
                occupant_id: "b".into()
            })
        );
        assert_eq!(r.focus_at(&occupants, 16), None);

        let overflow: Vec<_> = (0..20)
            .map(|i| Occupant {
                id: format!("a{i:02}"),
                occupancy: Occupancy::Idle,
            })
            .collect();
        let grid = r.grid(&overflow);
        assert_eq!(grid.len(), 16);
        assert_eq!(grid[15].map(|o| o.id.as_str()), Some("a15"));
        assert_eq!(r.focus_at(&overflow, 15).unwrap().occupant_id, "a15");
        assert_eq!(r.focus_at(&overflow, 16), None);

        let mut surface = RecordingSurface {
            caps,
            last: Vec::new(),
        };
        let model = AttentionModel {
            agents: vec![agent("done", AgentStatus::Done)],
            connected: true,
            ..AttentionModel::default()
        };
        paint(&mut surface, &model).unwrap();
        assert_eq!(surface.last[0].occupancy, Occupancy::Idle);
        assert_eq!(surface.caps().encoders, 9);
    }

    #[test]
    fn visible_uses_host_page_and_caps_cells() {
        let caps = MaschineCaps::mk3();
        let cells = caps.grid.cells();
        let agents: Vec<_> = (0..20)
            .map(|i| agent(&format!("a{i:02}"), AgentStatus::Idle))
            .collect();
        let mut model = AttentionModel {
            agents,
            connected: true,
            page: 1,
            ..AttentionModel::default()
        };
        model.sort_agents();
        let vis = OccupancySource::visible(&model, cells);
        assert_eq!(PAGE, 8);
        assert_eq!(cells, 16);
        assert_eq!(vis.len(), PAGE);
        assert_eq!(vis[0].id, "a08");
        assert_eq!(vis[7].id, "a15");
        assert_eq!(model.occupants().len(), 20);
        assert_eq!(
            Renderer::new(&caps)
                .grid(&vis)
                .iter()
                .filter(|s| s.is_some())
                .count(),
            PAGE
        );

        let mut surface = RecordingSurface {
            caps,
            last: Vec::new(),
        };
        paint(&mut surface, &model).unwrap();
        assert_eq!(surface.last.len(), PAGE);
        assert_eq!(surface.last[0].id, "a08");

        let r = Renderer::new(&surface.caps);
        assert_eq!(
            r.focus_at(&vis, 0),
            Some(Focus {
                occupant_id: "a08".into()
            })
        );
        assert_eq!(r.focus_at(&vis, PAGE), None);
        assert_eq!(r.focus_at(&vis, 15), None);
    }

    #[test]
    fn pad_slots_match_occupancy_visible() {
        let caps = MaschineCaps::mk3();
        let cells = caps.grid.cells();
        let agents: Vec<_> = (0..20)
            .map(|i| agent(&format!("a{i:02}"), AgentStatus::Idle))
            .collect();
        let mut model = AttentionModel {
            agents,
            connected: true,
            page: 1,
            ..AttentionModel::default()
        };
        model.sort_agents();
        let vis = OccupancySource::visible(&model, cells);
        let slots = model.pad_slots();
        assert_eq!(vis.len(), PAGE);
        assert_eq!(
            slots
                .iter()
                .flatten()
                .map(|a| a.pane_id.as_str())
                .collect::<Vec<_>>(),
            vis.iter().map(|o| o.id.as_str()).collect::<Vec<_>>()
        );
        let r = Renderer::new(&caps);
        assert_eq!(
            r.focus_at(&vis, 0).map(|f| f.occupant_id),
            model.select_pad(0).map(|a| a.pane_id.clone())
        );
        assert_eq!(r.focus_at(&vis, PAGE - 1).unwrap().occupant_id, "a15");
        assert!(model.select_pad(PAGE as u8).is_none());
        let grid = r.grid(&vis);
        assert_eq!(grid.len(), cells);
        assert!(grid[PAGE].is_none());
        assert!(grid[cells - 1].is_none());
        assert_eq!(r.focus_at(&vis, PAGE), None);
        assert_eq!(r.focus_at(&vis, cells - 1), None);
    }

    #[test]
    fn glance_fits_caps_smaller_than_page() {
        let caps = SurfaceCaps {
            grid: Grid { cols: 2, rows: 2 },
            lights: Lights::IndexedHueBright,
            displays: Vec::new(),
            encoders: 0,
            exclusive_usb: None,
            update_interval_us: MK3_UPDATE_INTERVAL_US,
        };
        let cells = caps.grid.cells();
        assert_eq!(cells, 4);
        let agents: Vec<_> = (0..20)
            .map(|i| agent(&format!("a{i:02}"), AgentStatus::Idle))
            .collect();
        let mut model = AttentionModel {
            agents,
            connected: true,
            page: 1,
            ..AttentionModel::default()
        };
        model.sort_agents();
        let vis = OccupancySource::visible(&model, cells);
        assert_eq!(vis.len(), cells);
        assert_eq!(vis[0].id, "a08");
        assert_eq!(vis[3].id, "a11");
        let r = Renderer::new(&caps);
        let grid = r.grid(&vis);
        assert_eq!(grid.len(), 4);
        assert!(grid.iter().all(|s| s.is_some()));
        assert_eq!(r.focus_at(&vis, 3).unwrap().occupant_id, "a11");
        assert_eq!(r.focus_at(&vis, 4), None);
        assert_eq!(OccupancySource::glance(&model, 0, cells)[0].id, "a00");
    }
}
