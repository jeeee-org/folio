//! ファイル操作の中核（作成・改名・ごみ箱）。ターミナルには触らない。
//! 失って困る操作（上書き・完全削除）はここには無い。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// `dir`の中に`name`を作る。末尾が`/`ならディレクトリ、それ以外は空のファイル。
/// 途中のディレクトリも作る。既にあれば作らずエラー。
pub fn create(dir: &Path, name: &str) -> Result<PathBuf> {
    let name = name.trim();
    if name.is_empty() || name == "/" {
        bail!("名前が空です");
    }
    if name.starts_with('/') || name.split('/').any(|p| p == "..") {
        bail!("絶対パスと`..`は使えません");
    }
    let is_dir = name.ends_with('/');
    let path = dir.join(name.trim_end_matches('/'));
    if path.exists() {
        bail!("{}は既にあります", path.display());
    }
    if is_dir {
        fs::create_dir_all(&path).with_context(|| format!("{}を作れません", path.display()))?;
    } else {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("{}を作れません", parent.display()))?;
        }
        fs::File::create_new(&path).with_context(|| format!("{}を作れません", path.display()))?;
    }
    Ok(path)
}

/// 同じディレクトリの中で名前を変える。変更先が既にあれば拒む（上書きしない）。
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        bail!("名前が空です");
    }
    if new_name.contains('/') {
        bail!("名前に`/`は使えません（移動はまだできません）");
    }
    let parent = path.parent().context("親ディレクトリがありません")?;
    let target = parent.join(new_name);
    if target == path {
        return Ok(target);
    }
    if target.exists() {
        bail!("{}は既にあります", target.display());
    }
    fs::rename(path, &target)
        .with_context(|| format!("{}を{}に改名できません", path.display(), target.display()))?;
    Ok(target)
}

/// ごみ箱の置き場所。`$XDG_DATA_HOME/Trash`、無ければ`~/.local/share/Trash`。
pub fn trash_root() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
        })?;
    Some(base.join("Trash"))
}

/// XDG Trashへ移す。移った先（`Trash/files/<名前>`）を返す。
pub fn trash(path: &Path) -> Result<PathBuf> {
    let root = trash_root().context("ごみ箱の場所が決められません（HOMEが未設定）")?;
    trash_into(path, &root)
}

/// `root`をごみ箱として移す。同じファイルシステムでなければ断る（コピーはしない）。
pub fn trash_into(path: &Path, root: &Path) -> Result<PathBuf> {
    let path =
        fs::canonicalize(path).with_context(|| format!("{}が見つかりません", path.display()))?;
    // 取り返しのつかない対象は最初から断る
    if path.parent().is_none() {
        bail!("ルートは捨てられません");
    }
    if let Some(home) = std::env::var_os("HOME")
        && path == Path::new(&home)
    {
        bail!("ホームディレクトリは捨てられません");
    }
    if root.starts_with(&path) {
        bail!("ごみ箱を含むディレクトリは捨てられません");
    }
    let files = root.join("files");
    let info = root.join("info");
    fs::create_dir_all(&files).with_context(|| format!("{}を作れません", files.display()))?;
    fs::create_dir_all(&info).with_context(|| format!("{}を作れません", info.display()))?;

    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "item".to_string());
    let (dest, info_path) = unique_slot(&files, &info, &base);

    // 先に情報を書く（移した後に書けないと元の場所が分からなくなる）
    let stamp = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
    let content = format!(
        "[Trash Info]\nPath={}\nDeletionDate={stamp}\n",
        percent_encode(&path.to_string_lossy())
    );
    fs::write(&info_path, content)
        .with_context(|| format!("{}に書けません", info_path.display()))?;

    if let Err(err) = fs::rename(&path, &dest) {
        if err.kind() != io::ErrorKind::CrossesDevices {
            let _ = fs::remove_file(&info_path);
            return Err(err).with_context(|| format!("{}をごみ箱へ移せません", path.display()));
        }
        // 別のファイルシステム: コピーしてから元を消す
        if let Err(err) = copy_tree(&path, &dest).and_then(|_| remove_any(&path)) {
            let _ = remove_any(&dest);
            let _ = fs::remove_file(&info_path);
            return Err(err).with_context(|| format!("{}をごみ箱へ移せません", path.display()));
        }
    }
    Ok(dest)
}

/// `dst_dir`の中へコピーする（同じ名前で）。ディレクトリは中身ごと。
/// 同名があれば`overwrite`が真の時だけ消してから置く。
pub fn copy_into(src: &Path, dst_dir: &Path, overwrite: bool) -> Result<PathBuf> {
    let target = prepare_target(src, dst_dir, overwrite)?;
    copy_tree(src, &target).with_context(|| format!("{}をコピーできません", src.display()))?;
    Ok(target)
}

/// `dst_dir`の中へ移動する。同じファイルシステムなら`rename`、違えばコピー＋削除。
pub fn move_into(src: &Path, dst_dir: &Path, overwrite: bool) -> Result<PathBuf> {
    let target = prepare_target(src, dst_dir, overwrite)?;
    match fs::rename(src, &target) {
        Ok(()) => Ok(target),
        Err(err) if err.kind() == io::ErrorKind::CrossesDevices => {
            if let Err(err) = copy_tree(src, &target).and_then(|_| remove_any(src)) {
                let _ = remove_any(&target);
                return Err(err).with_context(|| format!("{}を移動できません", src.display()));
            }
            Ok(target)
        }
        Err(err) => Err(err).with_context(|| format!("{}を移動できません", src.display())),
    }
}

/// 行き先を決める。自分自身・自分の中への操作は拒み、同名は`overwrite`に従う。
fn prepare_target(src: &Path, dst_dir: &Path, overwrite: bool) -> Result<PathBuf> {
    let name = src.file_name().context("名前のないパスです")?;
    let target = dst_dir.join(name);
    let src_abs =
        fs::canonicalize(src).with_context(|| format!("{}が見つかりません", src.display()))?;
    let dst_abs = fs::canonicalize(dst_dir)
        .with_context(|| format!("{}が見つかりません", dst_dir.display()))?;
    if dst_abs == src_abs || dst_abs.starts_with(&src_abs) {
        bail!("自分自身の中へは置けません");
    }
    if target.exists() || target.symlink_metadata().is_ok() {
        if fs::canonicalize(&target).ok() == Some(src_abs) {
            bail!("同じ場所です");
        }
        if !overwrite {
            bail!("{}は既にあります", target.display());
        }
        remove_any(&target).with_context(|| format!("{}を消せません", target.display()))?;
    }
    Ok(target)
}

/// ファイルはそのまま、ディレクトリは中身ごとコピーする。シンボリックリンクはリンクのまま。
fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        let link = fs::read_link(src)?;
        std::os::unix::fs::symlink(link, dst)?;
        return Ok(());
    }
    if meta.is_dir() {
        fs::create_dir(dst)?;
        for item in fs::read_dir(src)? {
            let item = item?;
            copy_tree(&item.path(), &dst.join(item.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(src, dst).map(|_| ())
    }
}

/// ファイルでもディレクトリでも消す（ごみ箱を通さない。コピー後の元や上書きの対象用）。
fn remove_any(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// 外部アプリで開く（`wslview`があればそれ、無ければ`xdg-open`）。使ったコマンド名を返す。
pub fn open_external(path: &Path) -> Result<String> {
    let candidates = ["wslview", "xdg-open"];
    for cmd in candidates {
        let spawned = std::process::Command::new(cmd)
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match spawned {
            Ok(_) => return Ok(cmd.to_string()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err).with_context(|| format!("{cmd}を起動できません")),
        }
    }
    bail!("wslviewもxdg-openも見つかりません")
}

/// `files/`と`info/`の両方で空いている名前を選ぶ（`name`、`name.2`、`name.3`…）。
fn unique_slot(files: &Path, info: &Path, base: &str) -> (PathBuf, PathBuf) {
    let mut n = 1;
    loop {
        let name = if n == 1 {
            base.to_string()
        } else {
            format!("{base}.{n}")
        };
        let dest = files.join(&name);
        let info_path = info.join(format!("{name}.trashinfo"));
        if !dest.exists() && !info_path.exists() {
            return (dest, info_path);
        }
        n += 1;
    }
}

/// trashinfoのPath用（RFC 2396のunreservedと`/`以外を%XXに）。
fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric() || b"-_.!~*'()/".contains(&b);
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("folio-fileops-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn create_file_dir_and_nested_and_refuse_existing() {
        let dir = sandbox("create");
        assert!(create(&dir, "a.md").unwrap().is_file());
        assert!(create(&dir, "sub/").unwrap().is_dir());
        assert!(create(&dir, "x/y/z.txt").unwrap().is_file());
        assert!(create(&dir, "a.md").is_err());
        assert!(create(&dir, "").is_err());
        assert!(create(&dir, "../escape").is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_refuses_overwrite_and_slash() {
        let dir = sandbox("rename");
        let a = create(&dir, "a.md").unwrap();
        create(&dir, "b.md").unwrap();
        assert!(rename(&a, "b.md").is_err());
        assert!(rename(&a, "sub/c.md").is_err());
        let c = rename(&a, "c.md").unwrap();
        assert!(c.exists() && !a.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn trash_moves_and_writes_info_with_unique_names() {
        let dir = sandbox("trash");
        let root = dir.join("Trash");
        let f = create(&dir, "doc.md").unwrap();
        fs::write(&f, "hello").unwrap();
        let moved = trash_into(&f, &root).unwrap();
        assert!(!f.exists());
        assert_eq!(moved, root.join("files").join("doc.md"));
        let info = fs::read_to_string(root.join("info").join("doc.md.trashinfo")).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath="));
        assert!(info.contains("DeletionDate="));
        // 同名をもう一度捨てると番号が付く
        let f2 = create(&dir, "doc.md").unwrap();
        let moved2 = trash_into(&f2, &root).unwrap();
        assert_eq!(moved2, root.join("files").join("doc.md.2"));
        assert!(root.join("info").join("doc.md.2.trashinfo").exists());
        // ディレクトリも捨てられる
        let d = create(&dir, "sub/").unwrap();
        assert!(trash_into(&d, &root).unwrap().is_dir());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn trash_refuses_dangerous_targets() {
        let dir = sandbox("danger");
        let root = dir.join("Trash");
        assert!(trash_into(Path::new("/"), &root).is_err());
        // ごみ箱を含むディレクトリ
        fs::create_dir_all(&root).unwrap();
        assert!(trash_into(&dir, &root).is_err());
        assert!(trash_into(&dir.join("missing"), &root).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn copy_and_move_handle_files_dirs_and_overwrite() {
        let dir = sandbox("copy");
        let src = create(&dir, "src/").unwrap();
        fs::write(src.join("a.txt"), "A").unwrap();
        fs::create_dir(src.join("inner")).unwrap();
        fs::write(src.join("inner/b.txt"), "B").unwrap();
        let dst = create(&dir, "dst/").unwrap();
        // ディレクトリを中身ごとコピー
        let copied = copy_into(&src, &dst, false).unwrap();
        assert_eq!(fs::read_to_string(copied.join("inner/b.txt")).unwrap(), "B");
        // 同名は拒む／上書きなら置き換える
        assert!(copy_into(&src, &dst, false).is_err());
        fs::write(src.join("a.txt"), "A2").unwrap();
        copy_into(&src, &dst, true).unwrap();
        assert_eq!(fs::read_to_string(copied.join("a.txt")).unwrap(), "A2");
        // 自分の中へは拒む
        assert!(copy_into(&src, &src.join("inner"), true).is_err());
        // ファイルの移動
        let f = create(&dir, "f.txt").unwrap();
        let moved = move_into(&f, &dst, false).unwrap();
        assert!(moved.exists() && !f.exists());
        // ディレクトリの移動（上書き）
        let moved_dir = move_into(&src, &dst, true).unwrap();
        assert!(moved_dir.join("inner/b.txt").exists() && !src.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn percent_encoding_keeps_slash_and_encodes_space_and_utf8() {
        assert_eq!(percent_encode("/a b/日"), "/a%20b/%E6%97%A5");
    }
}
