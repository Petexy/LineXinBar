//! What this machine's Steam client says is installed, read the way the shell
//! reads it.
//!
//! The disk half of the library, on its own and with no account involved: the
//! Steam libraries this machine has, the manifests inside them, and the rows
//! those turn into. Nothing here signs in, asks Steam anything, or touches the
//! network — so it is the one part of the integration that can be checked
//! against a real machine without an account.
//!
//! ```console
//! $ cargo run -p lxb-steam --example probe-library
//! ```
//!
//! Everything printed comes off the disk as it is. Nothing is invented for the
//! sake of having something to show: a machine with no Steam client on it
//! prints that it found none.

fn main() {
    let libraries = lxb_steam::library::libraries();
    match lxb_steam::library::root() {
        Some(root) => println!("Steam is at {}", root.display()),
        None => println!("no Steam client is installed on this machine"),
    }
    println!("{} librar{}:", libraries.len(), plural(libraries.len()));
    for library in &libraries {
        println!("  {}", library.display());
    }

    let installed = lxb_steam::library::installed();
    println!("\n{} installed:", installed.len());

    // Through the same merge the shell uses, with nothing owned, so what is
    // printed is the column the bar would build for somebody whose account
    // Steam cannot be asked about — which is also what a signed-out machine
    // with games on it shows.
    for game in lxb_steam::library::merge(Vec::new(), &installed) {
        println!(
            "  {:>8}  {:<52}  {}",
            game.app_id,
            elide(&game.name, 52),
            game.note()
        );
    }

    match lxb_steam::client::Where::find() {
        Some(where_it_is) => {
            println!("\nValve's client is here: {where_it_is:?}; it is what starts these.")
        }
        None => println!(
            "\nthere is no Valve client on this machine, so nothing in this library can be started"
        ),
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        "y"
    } else {
        "ies"
    }
}

/// Keep the columns lined up when a game has a very long name.
fn elide(name: &str, width: usize) -> String {
    if name.chars().count() <= width {
        return name.to_string();
    }
    name.chars().take(width - 1).collect::<String>() + "…"
}
