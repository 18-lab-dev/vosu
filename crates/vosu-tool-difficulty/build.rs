fn main() {
    println!("cargo:rerun-if-changed=vendor");
    println!("cargo:rerun-if-changed=src/minacalc.cpp");
    cc::Build::new()
        .cpp(true)
        .std("c++20")
        .define("STANDALONE_CALC", None)
        .include("vendor")
        .file("vendor/MinaCalc/MinaCalc.cpp")
        .file("src/minacalc.cpp")
        .warnings(false)
        .compile("vosu_minacalc");
}
