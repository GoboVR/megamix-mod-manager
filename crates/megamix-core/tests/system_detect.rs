//! Runs Steam detection against the real machine. Opt-in:
//! `cargo test -p megamix-core --test system_detect -- --ignored --nocapture`

#[test]
#[ignore = "depends on the local Steam install"]
fn detect_on_this_machine() {
    let roots = megamix_core::steam::steam_roots();
    println!("steam roots: {roots:#?}");
    for r in &roots {
        println!(
            "libraries of {}: {:#?}",
            r.display(),
            megamix_core::steam::library_folders(r)
        );
        for u in megamix_core::launch::read_all(r) {
            println!("launch options: {u:#?}");
            if !u.check.ok() {
                println!(
                    "suggested: {}",
                    megamix_core::launch::suggest(u.options.as_deref(), true)
                );
            }
        }
    }
    match megamix_core::steam::find_game(None) {
        Ok(g) => println!("found: {g:#?}"),
        Err(e) => println!("not found: {e}"),
    }
}
