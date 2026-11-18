//! The operator's two per-device choices in the top bar (shading#24, mockup theme-v4b): how the
//! cards are laid out and which colour mode the room needs. Both are remembered per device in
//! `localStorage` under [`LAYOUT_KEY`] / [`MODE_KEY`]; a missing, unknown or unreadable value is
//! the default (4+4 grid, dark), never an error.

/// The `localStorage` key of the card layout.
pub const LAYOUT_KEY: &str = "shading.layout";
/// The `localStorage` key of the colour mode.
pub const MODE_KEY: &str = "shading.mode";

/// How the camera cards stand on the screen (`data-layout` on `<html>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CardLayout {
    /// "4+4": a grid of 4 columns (2 under 1100 px, 1 on a phone); 8 cameras = 2 rows.
    #[default]
    Grid,
    /// "5": one row of exactly 5 whole cards (the usual 3+2 event set), the rest scrolls aside.
    Five,
    /// "8 v rade": one row of 4 1/3 cards (the third says there is more), scrolls aside.
    Row,
}

impl CardLayout {
    pub const ALL: [CardLayout; 3] = [CardLayout::Grid, CardLayout::Five, CardLayout::Row];

    /// The stored and `data-layout` word.
    pub fn as_str(self) -> &'static str {
        match self {
            CardLayout::Grid => "grid",
            CardLayout::Five => "five",
            CardLayout::Row => "row",
        }
    }

    /// The stored word back; anything else (missing, an older or foreign value) is the default.
    pub fn parse(stored: Option<&str>) -> Self {
        match stored {
            Some("five") => CardLayout::Five,
            Some("row") => CardLayout::Row,
            _ => CardLayout::Grid,
        }
    }

    /// The pill's text.
    pub fn label(self) -> &'static str {
        match self {
            CardLayout::Grid => "▦ 4+4",
            CardLayout::Five => "▥ 5",
            CardLayout::Row => "▤ 8 v rade",
        }
    }

    /// The pill's tooltip.
    pub fn title(self) -> &'static str {
        match self {
            CardLayout::Grid => "4 stĺpce, 8 kamier = 2 riadky",
            CardLayout::Five => "presne 5 celých kariet v rade (3+2), ďalšie sa posúvajú do strany",
            CardLayout::Row => "všetky kamery v jednom rade, posúva sa do strany",
        }
    }
}

/// The colour mode for the room's light (`data-mode` on `<html>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    /// "Tma": a darkened room, the default (low luminance, no pure white glare).
    #[default]
    Dark,
    /// "Svetlo": a lit room, a muted grey-blue paper (owner 18.11: no pure white).
    Light,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 2] = [ThemeMode::Light, ThemeMode::Dark];

    /// The stored and `data-mode` word.
    pub fn as_str(self) -> &'static str {
        match self {
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
        }
    }

    /// The stored word back; anything else is the default (dark).
    pub fn parse(stored: Option<&str>) -> Self {
        match stored {
            Some("light") => ThemeMode::Light,
            _ => ThemeMode::Dark,
        }
    }

    /// The pill's text.
    pub fn label(self) -> &'static str {
        match self {
            ThemeMode::Dark => "☾ Tma",
            ThemeMode::Light => "☀ Svetlo",
        }
    }

    /// The browser's `color-scheme` for the mode (form controls, scrollbars).
    pub fn color_scheme(self) -> &'static str {
        self.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layout_round_trips_through_its_stored_word() {
        for l in CardLayout::ALL {
            assert_eq!(CardLayout::parse(Some(l.as_str())), l);
        }
        assert_eq!(CardLayout::Five.as_str(), "five");
        assert_eq!(CardLayout::Row.as_str(), "row");
    }

    #[test]
    fn a_missing_or_foreign_layout_is_the_grid() {
        assert_eq!(CardLayout::parse(None), CardLayout::Grid);
        assert_eq!(CardLayout::parse(Some("")), CardLayout::Grid);
        assert_eq!(CardLayout::parse(Some("FIVE")), CardLayout::Grid);
        assert_eq!(CardLayout::parse(Some("8")), CardLayout::Grid);
    }

    #[test]
    fn a_mode_round_trips_and_defaults_to_dark() {
        for m in ThemeMode::ALL {
            assert_eq!(ThemeMode::parse(Some(m.as_str())), m);
        }
        assert_eq!(ThemeMode::parse(None), ThemeMode::Dark);
        assert_eq!(ThemeMode::parse(Some("sepia")), ThemeMode::Dark);
        assert_eq!(ThemeMode::Light.color_scheme(), "light");
    }

    #[test]
    fn the_pills_read_as_the_mockup() {
        let labels: Vec<&str> = CardLayout::ALL.iter().map(|l| l.label()).collect();
        assert_eq!(labels, ["▦ 4+4", "▥ 5", "▤ 8 v rade"]);
        let modes: Vec<&str> = ThemeMode::ALL.iter().map(|m| m.label()).collect();
        assert_eq!(modes, ["☀ Svetlo", "☾ Tma"]);
    }
}
