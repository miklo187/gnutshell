// Link against libmutter and its private Clutter/Cogl/Mtk libraries.
//
// The Clutter/Cogl/Mtk shared objects live in /usr/lib/mutter-<api>/, which is
// not on the default loader path, so we also embed an rpath.

const MUTTER_API: &str = "51";

fn main() {
    let libs = [
        format!("libmutter-{MUTTER_API}"),
        format!("mutter-clutter-{MUTTER_API}"),
        format!("mutter-cogl-{MUTTER_API}"),
        format!("mutter-mtk-{MUTTER_API}"),
        "polkit-agent-1".to_string(),
        "cairo".to_string(),
        "librsvg-2.0".to_string(),
    ];

    for name in &libs {
        let lib = pkg_config::Config::new()
            .cargo_metadata(true)
            .probe(name)
            .unwrap_or_else(|e| panic!("pkg-config could not find {name}: {e}"));

        for path in lib.link_paths {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", path.display());
        }
    }

    println!("cargo:rerun-if-changed=build.rs");
}
