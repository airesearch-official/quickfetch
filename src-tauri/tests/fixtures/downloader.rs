// Controlled child-process fixture for exit-code, stderr and recovery tests.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let folder = std::path::PathBuf::from(&args[args.iter().position(|arg| arg == "-P").unwrap() + 1]);
    let cookie = &args[args.iter().position(|arg| arg == "--cookies").unwrap() + 1];
    assert!(std::path::Path::new(cookie).is_file());
    println!("fixture: cookies argument accepted");
    let media = folder.join("source.mp4").to_string_lossy().into_owned();
    match args.last().unwrap().rsplit('/').next().unwrap() {
        "posterror" => {
            println!("__QF_DONE_JSON__{:?}", media);
            eprintln!("WARNING: a non-fatal cleanup warning");
            eprintln!("ERROR: post-download hook failed");
        }
        "recover" => {
            let target = folder.join("broken.webm").to_string_lossy().into_owned();
            println!("__QF_MERGE_JSON__{{\"inputs\":[{:?}],\"filename\":{:?}}}", media, target);
            eprintln!("ERROR: Postprocessing: incompatible WebM codec");
        }
        _ => eprintln!("ERROR: HTTP Error 429: Too Many Requests"),
    }
    std::process::exit(1);
}
