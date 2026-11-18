//! `<state>/shprd/web.json` and the QR code of the link.
//!
//! The file is what `shprd web status` and the web app's remote box read: whether
//! a tunnel is on, which provider, the link, a QR code of it and, when it is not
//! on, why. It holds the full link — a quick tunnel's address is what gets a
//! phone to the login page — so it is private (`0600`), and everything that
//! *shows* it masks it unless the person asked (`--show-url`).

use std::path::{Path, PathBuf};

use qrcode::QrCode;
use qrcode::render::{svg, unicode};
use serde::{Deserialize, Serialize};

use crate::error::RemoteError;
use crate::fsutil::write_private_atomic;

/// The record of the tunnel, as `web.json` holds it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebFile {
    /// Whether a tunnel is up.
    pub on: bool,
    /// Which provider, as its `--tunnel` word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The full link. A credential for a quick tunnel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// The link as an SVG QR code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qr_svg: Option<String>,
    /// Why there is no tunnel, when the last start or the last run ended badly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// When this was written, in epoch seconds.
    pub at: u64,
    /// The provider program's version line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl WebFile {
    /// Where the record lives under the service's state directory.
    pub fn path(state_dir: &Path) -> PathBuf {
        state_dir.join("web.json")
    }

    /// The record, or the "off" record when there is none or it cannot be read.
    ///
    /// A record that will not parse says "off": a stale file must never claim a
    /// tunnel that is not there.
    pub fn read(state_dir: &Path) -> WebFile {
        std::fs::read_to_string(Self::path(state_dir))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Writes the record, private and atomically.
    pub fn write(&self, state_dir: &Path) -> Result<(), RemoteError> {
        let json = serde_json::to_vec_pretty(self).map_err(|source| RemoteError::Json {
            what: "web.json",
            source,
        })?;
        write_private_atomic(&Self::path(state_dir), &json)
    }

    /// The record with the link and the QR code removed, which is how it is
    /// shown to anyone who did not ask for the address.
    pub fn masked(&self) -> WebFile {
        WebFile {
            link: self.link.as_deref().map(super::mask_url),
            qr_svg: None,
            ..self.clone()
        }
    }
}

/// The link as an SVG, or `None` when it is too long for a QR code.
pub fn qr_svg(link: &str) -> Option<String> {
    let code = QrCode::new(link.as_bytes()).ok()?;
    Some(
        code.render::<svg::Color>()
            .min_dimensions(220, 220)
            .quiet_zone(true)
            .dark_color(svg::Color("#000000"))
            .light_color(svg::Color("#ffffff"))
            .build(),
    )
}

/// The link as Unicode half-blocks, two QR rows to a line, for a terminal.
///
/// Dark modules are drawn as spaces on a light background, so a dark terminal
/// still shows a code a phone can read.
pub fn qr_unicode(link: &str) -> Option<String> {
    let code = QrCode::new(link.as_bytes()).ok()?;
    Some(
        code.render::<unicode::Dense1x2>()
            .dark_color(unicode::Dense1x2::Light)
            .light_color(unicode::Dense1x2::Dark)
            .quiet_zone(true)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_file_round_trips_and_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        assert!(!WebFile::read(dir.path()).on, "no file reads as off");
        let file = WebFile {
            on: true,
            provider: Some("cloudflared".into()),
            link: Some("https://a-b-c.trycloudflare.com".into()),
            qr_svg: Some("<svg/>".into()),
            why: None,
            at: 7,
            version: Some("cloudflared 1".into()),
        };
        file.write(dir.path()).unwrap();
        assert_eq!(WebFile::read(dir.path()), file);
        let mode = std::fs::metadata(WebFile::path(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_broken_file_reads_as_off() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(WebFile::path(dir.path()), "{not json").unwrap();
        assert!(!WebFile::read(dir.path()).on);
    }

    #[test]
    fn masked_has_no_address_and_no_code() {
        let file = WebFile {
            on: true,
            link: Some("https://a-b-c.trycloudflare.com".into()),
            qr_svg: Some("<svg/>".into()),
            ..WebFile::default()
        };
        let masked = file.masked();
        assert_eq!(
            masked.link.as_deref(),
            Some("https://****.trycloudflare.com")
        );
        assert_eq!(masked.qr_svg, None);
    }

    #[test]
    fn qr_codes_render_in_both_forms() {
        let link = "https://quiet-green-river.trycloudflare.com";
        let svg = qr_svg(link).unwrap();
        assert!(svg.contains("<svg"));
        let text = qr_unicode(link).unwrap();
        assert!(text.contains('█') || text.contains('▀') || text.contains('▄'));
        assert!(text.lines().count() > 10);
        // Too much for any QR version is no code, not a panic.
        assert_eq!(qr_svg(&"x".repeat(10_000)), None);
    }
}
