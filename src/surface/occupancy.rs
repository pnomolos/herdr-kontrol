//! Occupancy is the only status a device adapter paints.
//!
//! Host unread (`AgentStatus::Done`) collapses to [`Occupancy::Idle`] at the
//! runtime edge. Focus chrome, workspace, titles, and permission stay
//! host-local. [`OccupancySource`] is a separate edge from device adapters;
//! herdr is the only runtime for now.

/// Surface occupancy. Host Done collapses to [`Idle`] via `From<AgentStatus>`.
/// Rank is [`Self::attention_rank`], not derived `Ord` (variant order is not the API).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Occupancy {
    Blocked,
    Working,
    Idle,
    #[default]
    Unknown,
}

impl Occupancy {
    /// Lower is more attention. Blocked steals; Done is not a rank here.
    pub fn attention_rank(self) -> u8 {
        match self {
            Self::Blocked => 0,
            Self::Working => 1,
            Self::Idle => 2,
            Self::Unknown => 3,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Occupant {
    pub id: String,
    pub occupancy: Occupancy,
}

/// Glance+focus runtime action. Not host-local focus chrome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Focus {
    pub occupant_id: String,
}

impl From<&Occupant> for Focus {
    fn from(o: &Occupant) -> Self {
        Self {
            occupant_id: o.id.clone(),
        }
    }
}

/// Runtime edge. MK3 glance+focus only; no approve / send-keys.
pub trait OccupancySource {
    fn occupants(&self) -> Vec<Occupant>;

    fn focus(&self, id: &str) -> Option<Focus> {
        self.occupants()
            .iter()
            .find(|o| o.id == id)
            .map(Focus::from)
    }

    /// One page of occupants. Default stride is `cells`; host paging may differ.
    /// Empty cells are omitted; [`super::Renderer::grid`] maps them onto caps cells.
    fn glance(&self, page: usize, cells: usize) -> Vec<Occupant> {
        if cells == 0 {
            return Vec::new();
        }
        self.occupants()
            .into_iter()
            .skip(page.saturating_mul(cells))
            .take(cells)
            .collect()
    }

    fn visible(&self, cells: usize) -> Vec<Occupant> {
        self.glance(0, cells)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occupancy_attention_rank() {
        let mut v = [
            Occupancy::Unknown,
            Occupancy::Idle,
            Occupancy::Working,
            Occupancy::Blocked,
        ];
        v.sort_by_key(|o| o.attention_rank());
        assert_eq!(
            v,
            [
                Occupancy::Blocked,
                Occupancy::Working,
                Occupancy::Idle,
                Occupancy::Unknown,
            ]
        );
        assert!(Occupancy::Blocked.attention_rank() < Occupancy::Working.attention_rank());
        assert!(Occupancy::Working.attention_rank() < Occupancy::Idle.attention_rank());
        assert!(Occupancy::Idle.attention_rank() < Occupancy::Unknown.attention_rank());
    }

    struct OccupantsOnly(Vec<Occupant>);

    impl OccupancySource for OccupantsOnly {
        fn occupants(&self) -> Vec<Occupant> {
            self.0.clone()
        }
    }

    #[test]
    fn default_focus_searches_occupants() {
        let src = OccupantsOnly(vec![
            Occupant {
                id: "a".into(),
                occupancy: Occupancy::Working,
            },
            Occupant {
                id: "b".into(),
                occupancy: Occupancy::Blocked,
            },
        ]);
        assert_eq!(
            src.focus("b"),
            Some(Focus {
                occupant_id: "b".into()
            })
        );
        assert_eq!(src.focus("missing"), None);
        assert_eq!(src.glance(0, 1)[0].id, "a");
        assert_eq!(src.visible(1).len(), 1);
        assert!(src.glance(0, 0).is_empty());
        assert_eq!(src.glance(1, 1)[0].id, "b");
        assert_eq!(src.glance(1, 2).len(), 0);
    }
}
