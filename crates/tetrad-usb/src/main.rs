//! `tetrad-usb`: talk to Tetrad on a Quad Cortex over its USB cable (`docs/usb.md`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tetrad_usb::host::{Link, LinkError};
use tetrad_usb::{Answer, MAX_CHUNK_BYTES, Op};

const SHORT: Duration = Duration::from_secs(10);
/// A full chunk at the measured ~24 KiB/s host-to-unit rate takes about 3 s.
const CHUNK: Duration = Duration::from_secs(30);

#[derive(Parser)]
#[command(version, about = "Talk to Tetrad on a Quad Cortex over USB")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show the Tetrad version on the unit.
    Hello,
    /// Save the screen of the unit as a PNG.
    Screenshot {
        #[arg(short, long, default_value = "tetrad-screenshot.png")]
        output: PathBuf,
    },
    /// Print the recent notices of the unit.
    Log,
    /// Send a Tetrad release to the unit. `DIR` holds the release assets: `tetrad`, the three
    /// `tetrad-*.sh` scripts, `tetrad-update.json` and `tetrad-update.json.sig`. Then start the
    /// install on the unit: Settings > Tetrad > Tetrad Updates > UPDATE TETRAD.
    Install { dir: PathBuf },
    /// Send a JSON message to an installed extension with the `usb` capability and print its
    /// reply.
    Ext {
        /// The extension's manifest ID, such as `my.extension`.
        extension: String,
        /// A JSON value; default `null`.
        #[arg(default_value = "null")]
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error("{0}: {1}")]
    File(PathBuf, std::io::Error),
    #[error("unexpected reply: {0:?}")]
    Reply(Answer),
    #[error("the message is not JSON: {0}")]
    Json(#[from] serde_json::Error),
}

fn read(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(|error| Error::File(path.to_owned(), error))
}

fn run(command: Command) -> Result<(), Error> {
    let mut link = Link::open()?;
    match command {
        Command::Hello => match link.call(Op::Hello, &[], SHORT)? {
            (Answer::Hello { tetrad, protocol }, _) => {
                println!("Tetrad {tetrad} (USB protocol {protocol})");
            }
            (other, _) => return Err(Error::Reply(other)),
        },
        Command::Screenshot { output } => {
            let (_, png) = link.call(Op::Screenshot, &[], SHORT)?;
            std::fs::write(&output, png).map_err(|error| Error::File(output.clone(), error))?;
            println!("{}", output.display());
        }
        Command::Log => {
            let (_, text) = link.call(Op::Log, &[], SHORT)?;
            println!("{}", String::from_utf8_lossy(&text));
        }
        Command::Install { dir } => install(&mut link, &dir)?,
        Command::Ext { extension, message } => {
            let op = Op::Extension {
                extension,
                message: serde_json::from_str(&message)?,
            };
            match link.call(op, &[], SHORT)? {
                (Answer::Extension { value }, _) => println!("{value:#}"),
                (other, _) => return Err(Error::Reply(other)),
            }
        }
    }
    Ok(())
}

fn install(link: &mut Link, dir: &Path) -> Result<(), Error> {
    let text = |name: &str| -> Result<String, Error> {
        let bytes = read(&dir.join(name))?;
        String::from_utf8(bytes).map_err(|_| {
            let error = std::io::Error::new(std::io::ErrorKind::InvalidData, "not UTF-8");
            Error::File(dir.join(name), error)
        })
    };
    let begin = Op::InstallBegin {
        manifest: text("tetrad-update.json")?,
        signature: text("tetrad-update.json.sig")?,
    };
    let (Answer::InstallBegin { version, files }, _) = link.call(begin, &[], SHORT)? else {
        return Err(Error::Reply(Answer::Error {
            message: "no file list".into(),
        }));
    };
    println!("Sending Tetrad {version}");
    for file in files {
        // A release asset is named by the file name of its manifest path.
        let name = file.path.rsplit('/').next().unwrap_or(&file.path);
        let bytes = read(&dir.join(name))?;
        let mut offset = 0;
        for chunk in bytes.chunks(MAX_CHUNK_BYTES) {
            let op = Op::InstallChunk {
                path: file.path.clone(),
                offset,
            };
            match link.call(op, chunk, CHUNK)? {
                (Answer::InstallChunk { received, .. }, _) => offset = received,
                (other, _) => return Err(Error::Reply(other)),
            }
            print!("\r  {}: {offset} / {} bytes", file.path, file.size);
            let _ = std::io::stdout().flush();
        }
        println!();
    }
    link.call(Op::InstallReady, &[], SHORT)?;
    println!(
        "Tetrad {version} is on the unit. To install it, open Settings > Tetrad > Tetrad Updates \
         (Settings > System on 0.1.0-alpha.8 and earlier) on the unit and tap UPDATE TETRAD."
    );
    Ok(())
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tetrad-usb: {error}");
            ExitCode::FAILURE
        }
    }
}
