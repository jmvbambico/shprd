//! Regenerates `web/src/gen/` from the DTOs.
//!
//! Run it whenever a type in `crates/shprd-server/src/dto.rs` changes; the
//! `bindings_are_current` test fails until the output is committed.

use shprd_server::bindings;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = bindings::bindings_dir();
    let written = bindings::export(&dir)?;
    println!("wrote {} bindings to {}", written.len(), dir.display());
    Ok(())
}
