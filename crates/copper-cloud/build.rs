// The portal (`portal/out`, a Next.js static export) is embedded by rust-embed in release
// builds; rebuild when it changes. The folder always exists (a tracked `.gitkeep`).
fn main() {
    println!("cargo:rerun-if-changed=../../portal/out");
}
