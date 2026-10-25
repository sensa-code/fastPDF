//! What reading positions may do, decided from two facts only: whether
//! there is a store (a recent-files list) and whether "Remember position"
//! is on now. Every reader hook asks here, so turning the setting off (or
//! having no store) stops looking up, following and writing at once, even
//! for a document that started opening before the change.

use super::ReadingStart;

/// The two facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Gate {
    /// There is a positions store (a recent-files list).
    pub store: bool,
    /// The setting is on.
    pub remember: bool,
}

/// How a document that finished opening is followed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Follow {
    /// From the start the open found (restored or not).
    Start(ReadingStart),
    /// From where it is: the open did not ask the store (positions were off
    /// when it started), so its view is written like a moved one.
    Adopt,
    /// Not at all.
    Nothing,
}

impl Gate {
    fn on(self) -> bool {
        self.store && self.remember
    }

    /// An open may look the document up.
    pub(crate) fn lookup(self) -> bool {
        self.on()
    }

    /// Views may be noted, queued and written.
    pub(crate) fn write(self) -> bool {
        self.on()
    }

    /// How a document that finished opening with `start` is followed.
    pub(crate) fn follow(self, start: Option<ReadingStart>) -> Follow {
        match start {
            _ if !self.on() => Follow::Nothing,
            Some(start) => Follow::Start(start),
            None => Follow::Adopt,
        }
    }

    /// The start a document that finished opening keeps: none once
    /// positions are off, though the open found one before.
    pub(crate) fn keep(self, start: Option<ReadingStart>) -> Option<ReadingStart> {
        start.filter(|_| self.on())
    }
}

#[cfg(test)]
mod tests {
    use fastpdf_core::positions::{FileIdentity, Restore};
    use fastpdf_core::{SavedView, SavedZoom};
    use fastpdf_engine_api::{PageIndex, Rotation};

    use super::*;

    fn start() -> ReadingStart {
        ReadingStart {
            restore: Some(Restore::Exact(SavedView {
                page: PageIndex::new(9),
                offset: 0.25,
                x: 0.0,
                zoom: SavedZoom::FitWidth,
                rotation: Rotation::R0,
            })),
            identity: Some(FileIdentity {
                len: 1,
                modified: 2,
            }),
            remembered: true,
        }
    }

    #[test]
    fn no_store_or_setting_off_means_no_lookup_no_following_no_writes() {
        for gate in [
            Gate {
                store: false,
                remember: true,
            },
            Gate {
                store: true,
                remember: false,
            },
            Gate {
                store: false,
                remember: false,
            },
        ] {
            assert!(!gate.lookup(), "{gate:?}");
            assert!(!gate.write(), "{gate:?}");
            assert_eq!(gate.follow(Some(start())), Follow::Nothing, "{gate:?}");
            assert_eq!(gate.follow(None), Follow::Nothing, "{gate:?}");
            // A start found before the setting was turned off is dropped.
            assert_eq!(gate.keep(Some(start())), None, "{gate:?}");
        }
    }

    #[test]
    fn with_a_store_and_the_setting_on_everything_applies() {
        let gate = Gate {
            store: true,
            remember: true,
        };
        assert!(gate.lookup() && gate.write());
        assert_eq!(gate.follow(Some(start())), Follow::Start(start()));
        // Opened while positions were off, on now: followed from where it is.
        assert_eq!(gate.follow(None), Follow::Adopt);
        assert_eq!(gate.keep(Some(start())), Some(start()));
        assert_eq!(gate.keep(None), None);
    }
}
