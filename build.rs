use vergen::Emitter;
use vergen_git2::Git2Builder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let git2 = Git2Builder::default()
        .sha(true) // Enable SHA
        .build()?;

    Emitter::default().add_instructions(&git2)?.emit()?;

    Ok(())
}
