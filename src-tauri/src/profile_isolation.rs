// ───────────────────────────────────────────────────────────────────────────
// Profile cookie / localStorage isolation — single source of truth.
//
// Per ADR-0003, `Profile.profileId` is the cookie boundary: all tabs sharing
// a profile_id share cookies/localStorage; different profile_ids are fully
// isolated. Tauri 2.11 exposes two APIs that get us there:
//
//   - `WebviewBuilder::data_directory(PathBuf)` — honored by WebView2
//     (Windows) and WebKitGTK (Linux). Resolved relative to AppLocalData.
//   - `WebviewBuilder::data_store_identifier([u8; 16])` — honored by
//     WKWebView on macOS ≥ 14 (and iOS ≥ 17 for a future mobile bridge).
//     Required on macOS: `data_directory` is silently ignored there for
//     cookie/localStorage isolation, which is the bug this module fixes.
//
// `apply_profile_isolation` is the ONE place `cfg(target_os = "macos")`
// appears — `create_tab` calls it once and stays platform-agnostic.
// ───────────────────────────────────────────────────────────────────────────

use tauri::webview::WebviewBuilder;
use tauri::{Manager, Runtime};
use uuid::Uuid;

/// Sanctel's UUIDv5 namespace for Profile → data-store-identifier derivation.
///
/// The bytes themselves are arbitrary but MUST stay constant for the lifetime
/// of sanctel: changing them invalidates every existing Profile's
/// WKWebsiteDataStore on macOS, so all users lose their cookies / logged-in
/// sessions on the next launch. If a future migration ever needs this, it
/// has to be paired with an explicit data-store migration step.
const SANCTEL_PROFILE_NAMESPACE: Uuid = Uuid::from_bytes([
    0xd4, 0x9c, 0xa7, 0xe2, 0x1a, 0x6f, 0x4f, 0x88,
    0x9c, 0x4a, 0x73, 0x2b, 0x16, 0x05, 0x4e, 0x11,
]);

/// Map a `Profile.profileId` to a deterministic 16-byte identifier for
/// WKWebView's `data_store_identifier`.
///
/// Scheme: `UUIDv5(SANCTEL_PROFILE_NAMESPACE, profile_id.as_bytes())`. Pure
/// and stable across runs — same profile_id always yields the same 16 bytes,
/// which is the contract WKWebsiteDataStore needs so cookies survive sanctel
/// restart. Different profile_ids yield different UUIDs (UUIDv5's collision
/// resistance is bounded by SHA-1's, which is more than enough for the
/// handful of profiles a user actually creates).
pub fn profile_data_store_id(profile_id: &str) -> [u8; 16] {
    Uuid::new_v5(&SANCTEL_PROFILE_NAMESPACE, profile_id.as_bytes()).into_bytes()
}

/// Apply per-platform Profile isolation to a `WebviewBuilder`. The only place
/// in the codebase that branches on `target_os` for this concern.
///
/// - **macOS:** `data_store_identifier(profile_data_store_id(...))`. The
///   WKWebsiteDataStore is keyed to those bytes, so cookies / IndexedDB /
///   localStorage are fully isolated per Profile and survive restart. The
///   same API works on iOS ≥ 17; when sanctel ever ships a mobile bridge,
///   extend the cfg below to include `target_os = "ios"`.
/// - **Windows / Linux:** `data_directory(<app-local>/profiles/<profile_id>)`.
///   WebView2 and WebKitGTK both honor this for cookies / IndexedDB /
///   localStorage. The path is computed via `app.path().app_local_data_dir()`
///   so it lands in the OS-blessed per-app data directory.
pub fn apply_profile_isolation<R: Runtime, M: Manager<R>>(
    builder: WebviewBuilder<R>,
    profile_id: &str,
    app: &M,
) -> Result<WebviewBuilder<R>, String> {
    #[cfg(target_os = "macos")]
    {
        let _ = app;
        Ok(builder.data_store_identifier(profile_data_store_id(profile_id)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let local_data_dir = app
            .path()
            .app_local_data_dir()
            .map_err(|e| format!("app_local_data_dir resolution failed: {e}"))?;
        let profile_dir = local_data_dir.join("profiles").join(profile_id);
        Ok(builder.data_directory(profile_dir))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same input → same output, every time. The contract that lets cookies
    /// survive across sanctel restarts on macOS.
    #[test]
    fn profile_data_store_id_is_deterministic() {
        let a = profile_data_store_id("profile-default");
        let b = profile_data_store_id("profile-default");
        assert_eq!(a, b);
    }

    /// Different profile_ids → different identifiers. The contract that
    /// keeps Work cookies out of Personal webviews.
    #[test]
    fn profile_data_store_id_is_distinct_per_input() {
        let work = profile_data_store_id("profile-work");
        let personal = profile_data_store_id("profile-personal");
        assert_ne!(work, personal);
    }

    /// Empty string is a valid profile_id (degenerate but not crashable);
    /// must produce a stable 16-byte ID, not panic.
    #[test]
    fn profile_data_store_id_handles_empty_string() {
        let id = profile_data_store_id("");
        // Two calls match; the value is whatever UUIDv5(NS, b"") computes to.
        assert_eq!(id, profile_data_store_id(""));
    }

    /// Pin the exact bytes for one known profile_id so accidental changes
    /// to the namespace constant or the hashing scheme are caught at test
    /// time — without this, a future refactor could silently invalidate
    /// every existing user's macOS data store. Recompute only with an
    /// explicit migration plan.
    #[test]
    fn profile_data_store_id_pinned_for_default_profile() {
        // Literal — NOT re-derived from SANCTEL_PROFILE_NAMESPACE — so a
        // namespace edit fails this assertion instead of sliding through.
        let expected: [u8; 16] = [
            0x46, 0x88, 0x68, 0xe8, 0x6a, 0x7a, 0x55, 0x78,
            0x91, 0x69, 0x49, 0xf0, 0x79, 0x79, 0xe7, 0x19,
        ];
        let id = profile_data_store_id("profile-default");
        assert_eq!(id, expected);
        // Sanity: UUIDv5 sets version bits (byte 6 high nibble == 0x5) and
        // variant bits (byte 8 high two bits == 0b10).
        assert_eq!(id[6] & 0xf0, 0x50);
        assert_eq!(id[8] & 0xc0, 0x80);
    }
}
