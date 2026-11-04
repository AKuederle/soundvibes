fn main() {
    println!("cargo:rerun-if-changed=src/history_popup.cpp");
    let qt = pkg_config::Config::new()
        .atleast_version("6.2")
        .probe("Qt6Widgets")
        .expect("Qt6 Widgets development files are required (qt6-base-dev / qt6-base)");
    let mut build = cc::Build::new();
    build.cpp(true).std("c++17").file("src/history_popup.cpp");
    for path in qt.include_paths {
        build.include(path);
    }
    if std::env::var_os("CARGO_FEATURE_TEST_SUPPORT").is_some() {
        build.define("SV_TEST_SUPPORT", None);
    }
    build.compile("sv_history_popup");
}
