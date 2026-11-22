use std::{fs::OpenOptions, io::BufWriter, io::Write, path::PathBuf};

use lueying_rerank::Model;

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [source, stem, output] = &args[..] else {
        return Err(
            "usage: pack-model <ORT directory> <model stem> <new .packed.safetensors file>".into(),
        );
    };
    let stem = stem.to_str().ok_or("invalid model stem")?;
    let output = PathBuf::from(output);
    let mut bytes = Vec::new();
    Model::pack_keystroke_dir(&PathBuf::from(source), stem, &mut bytes)?;
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&output)
        .map_err(|error| format!("{}: {error}", output.display()))?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(&bytes)
        .and_then(|()| writer.flush())
        .map_err(|error| error.to_string())
}
