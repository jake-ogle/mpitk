use mpi::traits::*;
use std::collections::VecDeque;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::UNIX_EPOCH;

const OUTPUT_HEADER: &str = "type\tsize_bytes\thuman_readable_size\tmodified_unix\tfilepath";

#[derive(Clone, Copy)]
enum BundleMode {
    MaxFiles(usize),
    MaxBytes(u64),
}

fn parse_size(value: &str) -> Result<u64, String> {
    let digit_count = value.bytes().take_while(u8::is_ascii_digit).count();
    if digit_count == 0 {
        return Err(format!("invalid bundle size: {value}"));
    }
    let amount = value[..digit_count]
        .parse::<u64>()
        .map_err(|_| format!("invalid bundle size: {value}"))?;
    let factor = match value[digit_count..].to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kib" => 1 << 10,
        "kb" => 1_000,
        "m" | "mib" => 1 << 20,
        "mb" => 1_000_000,
        "g" | "gib" => 1 << 30,
        "gb" => 1_000_000_000,
        "t" | "tib" => 1 << 40,
        "tb" => 1_000_000_000_000,
        "p" | "pib" => 1 << 50,
        "pb" => 1_000_000_000_000_000,
        "e" | "eib" => 1 << 60,
        "eb" => 1_000_000_000_000_000_000,
        _ => return Err(format!("unsupported bundle size unit: {value}")),
    };
    let bytes = amount
        .checked_mul(factor)
        .ok_or_else(|| format!("bundle size is too large: {value}"))?;
    if bytes == 0 {
        return Err("bundle size must be greater than zero".to_owned());
    }
    Ok(bytes)
}

fn add_bundle_ids(entries: Vec<String>, mode: BundleMode) -> Vec<String> {
    let mut bundle_id = 1usize;
    let mut bundle_count = 0usize;
    let mut bundle_bytes = 0u64;
    entries
        .into_iter()
        .map(|entry| {
            let mut columns = entry.splitn(5, '\t');
            let kind = columns.next().unwrap_or_default();
            let size = columns.nth(0).and_then(|value| value.parse::<u64>().ok());
            let is_file = matches!(kind, "file" | "symlink");
            let assigned_id = if is_file {
                let size = size.unwrap_or(0);
                let start_new = match mode {
                    BundleMode::MaxFiles(limit) => bundle_count >= limit,
                    BundleMode::MaxBytes(limit) => {
                        bundle_count > 0 && bundle_bytes.saturating_add(size) > limit
                    }
                };
                if start_new {
                    bundle_id += 1;
                    bundle_count = 0;
                    bundle_bytes = 0;
                }
                bundle_count += 1;
                bundle_bytes = bundle_bytes.saturating_add(size);
                bundle_id.to_string()
            } else {
                "-".to_owned()
            };
            format!("{assigned_id}\t{entry}")
        })
        .collect()
}

fn human_readable_size(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn describe_entry(path: &Path, kind: &str) -> String {
    let display_path = path
        .to_string_lossy()
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    match fs::metadata(path) {
        Ok(metadata) => {
            let size = metadata.len();
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs().to_string())
                .unwrap_or_else(|| "-".to_owned());
            format!(
                "{kind}\t{size}\t{}\t{modified}\t{display_path}",
                human_readable_size(size)
            )
        }
        Err(error) => {
            eprintln!("{display_path}: {error}");
            format!("error\t-\t-\t-\t{display_path}")
        }
    }
}

const REQUEST_TAG: i32 = 10;
const RESPONSE_TAG: i32 = 11;
const RESULT_TAG: i32 = 12;

fn encode_fields(fields: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for field in fields {
        let value = field.as_bytes();
        bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
        bytes.extend_from_slice(value);
    }
    bytes
}

fn decode_fields(bytes: &[u8]) -> Option<Vec<String>> {
    let mut fields = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let end = offset.checked_add(8)?;
        let length = u64::from_le_bytes(bytes.get(offset..end)?.try_into().ok()?);
        offset = end;
        let end = offset.checked_add(usize::try_from(length).ok()?)?;
        fields.push(String::from_utf8(bytes.get(offset..end)?.to_vec()).ok()?);
        offset = end;
    }
    Some(fields)
}

fn scan_directory(path: &Path) -> (Vec<String>, Vec<String>) {
    let mut entries = Vec::new();
    let mut directories = Vec::new();
    match fs::read_dir(path) {
        Ok(iter) => {
            for item in iter {
                match item {
                    Ok(item) => {
                        let item_path = item.path();
                        let kind = match item.file_type() {
                            Ok(file_type) if file_type.is_dir() => {
                                directories.push(item_path.to_string_lossy().into_owned());
                                "directory"
                            }
                            Ok(file_type) if file_type.is_file() => "file",
                            Ok(file_type) if file_type.is_symlink() => "symlink",
                            Ok(_) => "other",
                            Err(error) => {
                                eprintln!("{}: {error}", item_path.display());
                                "unknown"
                            }
                        };
                        entries.push(describe_entry(&item_path, kind));
                    }
                    Err(error) => eprintln!("{}: {error}", path.display()),
                }
            }
        }
        Err(error) => eprintln!("{}: {error}", path.display()),
    }
    (entries, directories)
}

fn sort_entries(entries: &mut [String]) {
    entries.sort_by(|left, right| left.rsplit('\t').next().cmp(&right.rsplit('\t').next()));
}

fn coordinator(world: &impl Communicator, root: PathBuf) -> Vec<String> {
    let mut pending = VecDeque::from([root.to_string_lossy().into_owned()]);
    let mut waiting = Vec::new();
    let mut results = Vec::new();
    let mut in_flight = 0;
    let mut stopped = 0;
    while stopped < world.size() - 1 {
        let (message, status) = world.any_process().receive_vec::<u8>();
        let source = status.source_rank();
        if status.tag() == RESULT_TAG {
            in_flight -= 1;
            if let Some(fields) = decode_fields(&message) {
                if fields.first().map(String::as_str) == Some("result") && fields.len() >= 2 {
                    if let Ok(directory_count) = fields[1].parse::<usize>() {
                        let directory_end = 2usize.saturating_add(directory_count);
                        if directory_end <= fields.len() {
                            pending.extend(fields[2..directory_end].iter().cloned());
                            results.extend(fields[directory_end..].iter().cloned());
                        }
                    }
                }
            }
        }
        if let Some(directory) = pending.pop_front() {
            world.process_at_rank(source).send_with_tag(
                &encode_fields(&["task".to_owned(), directory]),
                RESPONSE_TAG,
            );
            in_flight += 1;
        } else if in_flight == 0 {
            world
                .process_at_rank(source)
                .send_with_tag(&encode_fields(&["stop".to_owned()]), RESPONSE_TAG);
            stopped += 1;
            for rank in waiting.drain(..) {
                world
                    .process_at_rank(rank)
                    .send_with_tag(&encode_fields(&["stop".to_owned()]), RESPONSE_TAG);
                stopped += 1;
            }
        } else {
            waiting.push(source);
        }
        while !waiting.is_empty() && !pending.is_empty() {
            let rank = waiting.remove(0);
            let directory = pending.pop_front().expect("queue checked as non-empty");
            world.process_at_rank(rank).send_with_tag(
                &encode_fields(&["task".to_owned(), directory]),
                RESPONSE_TAG,
            );
            in_flight += 1;
        }
    }
    sort_entries(&mut results);
    results
}

fn worker(world: &impl Communicator) {
    let coordinator = world.process_at_rank(0);
    coordinator.send_with_tag(&encode_fields(&["request".to_owned()]), REQUEST_TAG);
    loop {
        let (message, _) = coordinator.receive_vec_with_tag::<u8>(RESPONSE_TAG);
        let Some(fields) = decode_fields(&message) else {
            break;
        };
        match fields.first().map(String::as_str) {
            Some("task") if fields.len() == 2 => {
                let (entries, directories) = scan_directory(Path::new(&fields[1]));
                let mut result = vec!["result".to_owned(), directories.len().to_string()];
                result.extend(directories);
                result.extend(entries);
                coordinator.send_with_tag(&encode_fields(&result), RESULT_TAG);
            }
            Some("stop") => break,
            _ => break,
        }
    }
}

fn parse_args() -> Result<(PathBuf, Option<PathBuf>, Option<BundleMode>), String> {
    let mut args = env::args().skip(1);
    let usage = "usage: mpitk-ls <path> [-print | -dump <outputfile>] [-bundle-files <count> | -bundle-size <size>]";
    let path = args.next().ok_or_else(|| usage.to_owned())?;
    let mut dump = None;
    let mut bundle_mode = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-print" if dump.is_none() => {}
            "-dump" if dump.is_none() => {
                dump = Some(PathBuf::from(
                    args.next().ok_or("-dump requires an output file")?,
                ));
            }
            "-bundle-files" if bundle_mode.is_none() => {
                let value = args
                    .next()
                    .ok_or("-bundle-files requires a positive count")?;
                let count = value
                    .parse::<usize>()
                    .ok()
                    .filter(|count| *count > 0)
                    .ok_or("-bundle-files count must be greater than zero")?;
                bundle_mode = Some(BundleMode::MaxFiles(count));
            }
            "-bundle-size" if bundle_mode.is_none() => {
                let value = args.next().ok_or("-bundle-size requires a size")?;
                bundle_mode = Some(BundleMode::MaxBytes(parse_size(&value)?));
            }
            _ => return Err(format!("unexpected or conflicting argument: {arg}")),
        }
    }
    Ok((PathBuf::from(path), dump, bundle_mode))
}

fn run() -> Result<(), String> {
    let (path, dump, bundle_mode) = parse_args()?;
    if !path.is_dir() {
        return Err(format!("not a directory: {}", path.display()));
    }
    let universe = mpi::initialize().ok_or_else(|| "MPI is already initialized".to_owned())?;
    let world = universe.world();
    let entries = if world.rank() == 0 {
        if world.size() == 1 {
            let mut pending = vec![path.to_string_lossy().into_owned()];
            let mut entries = Vec::new();
            while let Some(directory) = pending.pop() {
                let (found, directories) = scan_directory(Path::new(&directory));
                entries.extend(found);
                pending.extend(directories);
            }
            sort_entries(&mut entries);
            entries
        } else {
            coordinator(&world, path)
        }
    } else {
        worker(&world);
        Vec::new()
    };
    if world.rank() == 0 {
        let (header, entries) = match bundle_mode {
            Some(mode) => (
                format!("bundle_id\t{OUTPUT_HEADER}"),
                add_bundle_ids(entries, mode),
            ),
            None => (OUTPUT_HEADER.to_owned(), entries),
        };
        let output = if entries.is_empty() {
            format!("{header}\n")
        } else {
            format!("{header}\n{}\n", entries.join("\n"))
        };
        if let Some(file) = dump {
            fs::write(&file, output).map_err(|error| format!("{}: {error}", file.display()))?;
        } else {
            print!("{output}");
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
