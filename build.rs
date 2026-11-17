use std::{
    env,
    ffi::OsString,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/bs-podcasts.ico");
    println!("cargo:rerun-if-env-changed=WINDRES");
    println!("cargo:rerun-if-env-changed=RC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let target = env::var("TARGET").expect("Cargo sets TARGET");
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let icon = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets manifest path"))
        .join("assets/bs-podcasts.ico");
    assert!(
        icon.is_file(),
        "required Windows application icon is missing"
    );
    // RC accepts forward slashes on Windows too. Avoid canonicalize(), whose
    // Windows verbatim-path prefix is not accepted by all resource compilers.
    let quoted_icon = icon
        .to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"");
    let rc_file = out_dir.join("bs-player-icon.rc");
    let version = env::var("CARGO_PKG_VERSION").expect("Cargo sets package version");
    let part = |name| {
        env::var(name)
            .expect("Cargo sets version components")
            .parse::<u16>()
            .expect("Windows version component fits u16")
    };
    let (major, minor, patch) = (
        part("CARGO_PKG_VERSION_MAJOR"),
        part("CARGO_PKG_VERSION_MINOR"),
        part("CARGO_PKG_VERSION_PATCH"),
    );
    fs::write(
        &rc_file,
        format!(
            r#"1 ICON "{quoted_icon}"
1 VERSIONINFO
 FILEVERSION {major},{minor},{patch},0
 PRODUCTVERSION {major},{minor},{patch},0
 FILEFLAGSMASK 0x3fL
 FILEFLAGS 0x0L
 FILEOS 0x40004L
 FILETYPE 0x1L
 FILESUBTYPE 0x0L
BEGIN
 BLOCK "StringFileInfo"
 BEGIN
  BLOCK "040904B0"
  BEGIN
   VALUE "FileDescription", "BS Player\0"
   VALUE "FileVersion", "{version}\0"
   VALUE "ProductName", "BS Player\0"
   VALUE "ProductVersion", "{version}\0"
   VALUE "InternalName", "bs-player\0"
   VALUE "OriginalFilename", "bs-player.exe\0"
  END
 END
 BLOCK "VarFileInfo"
 BEGIN
  VALUE "Translation", 0x0409, 1200
 END
END
"#
        ),
    )
    .expect("write Windows application resources");

    let resource = if target_env == "gnu" && target.starts_with("x86_64-") {
        compile_gnu_resource(&rc_file, &out_dir)
    } else if target_env == "msvc" {
        compile_msvc_resource(&rc_file, &out_dir)
    } else {
        panic!("Windows icon resource compilation is unsupported for {target}");
    };
    println!("cargo:rustc-link-arg-bin=bs-player={}", resource.display());
}

fn compile_gnu_resource(rc_file: &Path, out_dir: &Path) -> PathBuf {
    let resource = out_dir.join("bs-player-icon.o");
    let commands = match env::var_os("WINDRES") {
        Some(command) => vec![command],
        None => vec![
            OsString::from("x86_64-w64-mingw32-windres"),
            OsString::from("windres"),
        ],
    };
    for command in commands {
        match Command::new(&command)
            .arg("--target=pe-x86-64")
            .arg("--input-format=rc")
            .arg("--output-format=coff")
            .arg("-i")
            .arg(rc_file)
            .arg("-o")
            .arg(&resource)
            .status()
        {
            Ok(status) => {
                assert!(
                    status.success(),
                    "Windows icon resource compiler {command:?} failed: {status}"
                );
                return resource;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => panic!("could not run Windows icon compiler {command:?}: {error}"),
        }
    }
    panic!("Windows icon requires MinGW windres; set WINDRES to the resource compiler path");
}

fn compile_msvc_resource(rc_file: &Path, out_dir: &Path) -> PathBuf {
    let resource = out_dir.join("bs-player-icon.res");
    let command = env::var_os("RC").unwrap_or_else(|| "rc.exe".into());
    let status = Command::new(&command)
        .arg("/nologo")
        .arg("/fo")
        .arg(&resource)
        .arg(rc_file)
        .status()
        .unwrap_or_else(|error| {
            panic!("Windows icon requires the SDK resource compiler; set RC to its path: {error}")
        });
    assert!(
        status.success(),
        "Windows icon resource compiler {command:?} failed: {status}"
    );
    resource
}
