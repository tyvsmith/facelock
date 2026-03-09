pub fn run(text_only: bool) -> anyhow::Result<()> {
    if text_only {
        println!("Text-only preview mode is not yet implemented.");
        println!("This will print face detection results to stdout.");
    } else {
        println!("Graphical preview requires the Wayland preview module (spec 08).");
        println!("Use --text-only for text-based output.");
    }

    // Preview is delegated to spec 08 (Wayland preview window).
    // For now, provide a helpful message.
    Ok(())
}
