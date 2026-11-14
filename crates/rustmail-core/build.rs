//! Re-run compilation when migrations change so `sqlx::migrate!` stays fresh.

fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
