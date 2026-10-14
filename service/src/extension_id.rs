/// The Chrome extension's pinned ID — deterministic from the `key` field in
/// `extension/manifest.json` (backed by `extension/dev-signing-key.pem`).
/// Shared by everything that needs to name the extension to Chrome:
/// `native_host_install.rs` (Native Messaging `allowed_origins`) and
/// `registry_policy.rs` (the `ExtensionSettings` policy key).
pub const EXTENSION_ID: &str = "pjnjbihjibbooihkcdmcdjdpbbmkfdkk";
