#![forbid(unsafe_code)]

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let root = "src/display_stream/apollo";
    println!("cargo:rerun-if-changed={root}");
    cc::Build::new()
        .cpp(true)
        .static_crt(true)
        .std("c++20")
        .define("NOMINMAX", None)
        .define("WIN32_LEAN_AND_MEAN", None)
        .flag_if_supported("/utf-8")
        .flag_if_supported("/EHsc")
        .include(format!("{root}/adapters"))
        .include(format!("{root}/headers"))
        .include(format!("{root}/upstream"))
        .include(format!("{root}/upstream/moonlight-common-c/reedsolomon"))
        .file(format!("{root}/bridge.cpp"))
        .file(format!("{root}/transport.cpp"))
        .file(format!("{root}/upstream/src/nvenc/nvenc_base.cpp"))
        .file(format!("{root}/upstream/src/nvenc/nvenc_d3d11.cpp"))
        .file(format!("{root}/upstream/src/nvenc/nvenc_d3d11_native.cpp"))
        .file(format!("{root}/upstream/src/nvenc/nvenc_utils.cpp"))
        .compile("ferry_apollo_nvenc");
    cc::Build::new()
        .static_crt(true)
        .define("NOMINMAX", None)
        .define("SCREENFERRY_STANDALONE_QUEUE", None)
        .include(root)
        .include(format!("{root}/upstream/moonlight-common-c/reedsolomon"))
        .file(format!(
            "{root}/upstream/moonlight-common-c/reedsolomon/rs.c"
        ))
        .file(format!(
            "{root}/upstream/moonlight-common-c/src/RtpVideoQueue.c"
        ))
        .compile("ferry_moonlight_queue");
    println!("cargo:rustc-link-lib=dxguid");
    println!("cargo:rustc-link-lib=ws2_32");
    println!("cargo:rustc-link-lib=bcrypt");
}
