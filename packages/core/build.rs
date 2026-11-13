use std::env;
use std::fs;
use std::path::Path;

fn main() {
    tauri_build::build();

    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let dir = Path::new(&manifest).join("migrations");
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut migrations = Vec::new();
    for entry in
        fs::read_dir(&dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
    {
        let path = entry.unwrap().path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("sql") {
            continue;
        }

        let file = path.file_name().unwrap().to_str().unwrap();
        assert!(
            file.chars()
                .all(|value| value.is_ascii_alphanumeric() || matches!(value, '_' | '-' | '.')),
            "unsupported migration filename: {file}"
        );
        println!("cargo:rerun-if-changed={}", path.display());

        let stem = path.file_stem().unwrap().to_str().unwrap();
        let (version, name) = stem
            .split_once("__")
            .unwrap_or_else(|| panic!("migration must be V<number>__<name>.sql: {file}"));
        let version: i64 = version
            .strip_prefix('V')
            .unwrap_or_else(|| panic!("migration must start with V: {file}"))
            .parse()
            .unwrap_or_else(|_| panic!("invalid migration version: {file}"));
        migrations.push((version, name.to_string(), file.to_string()));
    }

    migrations.sort_by_key(|migration| migration.0);
    assert!(!migrations.is_empty(), "no migrations in {}", dir.display());
    assert!(
        migrations.iter().any(|migration| migration.0 == 0),
        "missing V0 baseline"
    );
    for pair in migrations.windows(2) {
        assert_ne!(
            pair[0].0, pair[1].0,
            "duplicate migration version {}",
            pair[0].0
        );
    }

    let mut out = String::from("static MIGRATIONS: &[Migration] = &[\n");
    for (version, name, file) in migrations {
        out.push_str(&format!(
            "    Migration {{ version: {version}, name: {name:?}, sql: include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/migrations/{file}\")) }},\n"
        ));
    }
    out.push_str("];\n");
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("migrations.rs"),
        out,
    )
    .unwrap();
}
