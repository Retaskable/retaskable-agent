//! The settings page (agent-settings.md section 4): `core` is the pure settings logic
//! (TypeScript's `machineSettings.ts`); `server` is the HTTP layer around it (`settingsServer.ts`).
pub mod core;
pub mod server;
