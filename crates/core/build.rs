// `sqlx::migrate!` embeds the migration files at compile time; make sure a new or edited
// migration (including the canvas crate's 0100+ files) triggers a rebuild.
fn main() {
    println!("cargo:rerun-if-changed=../copper-cloud/migrations");
}
