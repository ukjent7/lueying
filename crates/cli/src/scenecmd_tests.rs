//! 场景打包与安装的回路测试。

use super::*;
use std::fs;

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lueying-scene-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("建根目录");
    dir
}

const SCENE: &str = "\
[[nodes]]
id = \"glow\"
kind = \"shape\"
shape = \"rect\"
anchor = \"panel\"
fill = \"accent\"
";

fn write_scene(dir: &Path) {
    fs::create_dir_all(dir.join("assets")).expect("建场景目录");
    fs::write(dir.join("scene.toml"), SCENE).expect("写 scene.toml");
    fs::write(
        dir.join("assets").join("glow.png"),
        [0x89, b'P', b'N', b'G'],
    )
    .expect("写素材");
}

#[test]
fn pack_and_install_roundtrip() {
    let root = temp_root("roundtrip");
    let source = root.join("glow");
    write_scene(&source);
    let package = pack(&ScenePackArgs {
        dir: source.clone(),
        out: None,
    })
    .unwrap();
    assert_eq!(package, root.join("glow.luext"));
    let packed = fs::read(&package).unwrap();
    assert!(String::from_utf8_lossy(&packed).starts_with("### lueying-pack v2\n"));
    assert!(String::from_utf8_lossy(&packed).contains("### binary: assets/glow.png 4\n"));

    let target = root.join("scene-root");
    fs::create_dir_all(&target).unwrap();
    let installed = install(&SceneInstallArgs {
        source: package,
        dir: Some(target.clone()),
        force: false,
    })
    .unwrap();
    assert_eq!(installed, target);
    assert_eq!(
        fs::read_to_string(target.join("scene.toml")).unwrap(),
        SCENE
    );
    assert_eq!(
        fs::read(target.join("assets").join("glow.png")).unwrap(),
        [0x89, b'P', b'N', b'G']
    );
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn install_refuses_overwrite_without_force() {
    let root = temp_root("overwrite");
    let source = root.join("glow");
    write_scene(&source);
    let target = root.join("scene-root");
    fs::create_dir_all(&target).unwrap();
    let package = pack(&ScenePackArgs {
        dir: source,
        out: None,
    })
    .unwrap();
    let args = SceneInstallArgs {
        source: package,
        dir: Some(target.clone()),
        force: false,
    };
    install(&args).unwrap();
    assert!(install(&args).is_err());
    assert!(
        install(&SceneInstallArgs {
            force: true,
            ..args
        })
        .is_ok()
    );
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn rejects_bad_or_missing_scene() {
    let root = temp_root("reject");
    // 打包前校验:坏 scene.toml 不出包。
    let source = root.join("bad");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("scene.toml"), "version = 1\n[node.x]\n").unwrap();
    let error = pack(&ScenePackArgs {
        dir: source.clone(),
        out: None,
    })
    .unwrap_err();
    assert!(!error.is_empty(), "{error}");
    // 目录里没有 scene.toml 也不出包。
    let empty = root.join("empty");
    fs::create_dir_all(&empty).unwrap();
    fs::write(empty.join("readme.md"), "x").unwrap();
    assert!(
        pack(&ScenePackArgs {
            dir: empty,
            out: None
        })
        .is_err()
    );
    // 手工构造缺 scene.toml 的包,安装侧拒绝。
    let files: Files = vec![("assets/glow.png".into(), vec![1, 2, 3])];
    let bytes = container::encode(&files).unwrap();
    let target = root.join("scene-root");
    fs::create_dir_all(&target).unwrap();
    let error = install(&SceneInstallArgs {
        source: root.join("fake.luext"),
        dir: Some(target),
        force: false,
    })
    .unwrap_err();
    // source 不存在先报读失败;改用真实字节再验内容判定。
    assert!(
        error.contains("fake.luext") || error.contains("scene.toml"),
        "{error}"
    );
    fs::write(root.join("fake.luext"), &bytes).unwrap();
    let target = root.join("scene-root");
    assert!(
        install(&SceneInstallArgs {
            source: root.join("fake.luext"),
            dir: Some(target),
            force: false,
        })
        .unwrap_err()
        .contains("scene.toml")
    );
    fs::remove_dir_all(&root).unwrap();
}
