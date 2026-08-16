use std::process::Command;

fn compile_helper(source: &str, name: &str, frameworks: &[&str]) {
    println!("cargo:rerun-if-changed={source}");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let dest = format!("{out_dir}/{name}");
    let mut args = vec!["-fobjc-arc".to_string(), "-O2".to_string()];
    for f in frameworks {
        args.push("-framework".into());
        args.push((*f).into());
    }
    args.push("-o".into());
    args.push(dest);
    args.push(source.into());
    let status = Command::new("clang")
        .args(&args)
        .status()
        .expect("clang not found — install Xcode Command Line Tools (xcode-select --install)");
    assert!(status.success(), "failed to compile {name}");
}

fn main() {
    compile_helper(
        "ocr/main.m",
        "anveesa-ocr",
        &["Foundation", "AppKit", "Vision"],
    );
    compile_helper(
        "audio/main.m",
        "anveesa-audio",
        &["Foundation", "ScreenCaptureKit", "CoreMedia", "CoreAudio"],
    );
    compile_helper("doc/main.m", "anveesa-doc", &["Foundation", "PDFKit"]);
    compile_helper(
        "pdfmask/main.m",
        "anveesa-pdfmask",
        &["Foundation", "PDFKit", "AppKit"],
    );
}
