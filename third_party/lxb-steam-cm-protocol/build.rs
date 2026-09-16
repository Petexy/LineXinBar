use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = PathBuf::from("proto");
    let protos = [
        proto_dir.join("enums.proto"),
        proto_dir.join("encrypted_app_ticket.proto"),
        proto_dir.join("steammessages_base.proto"),
        proto_dir.join("steammessages_unified_base.steamclient.proto"),
        proto_dir.join("steammessages_auth.steamclient.proto"),
        proto_dir.join("steammessages_clientserver_login.proto"),
        proto_dir.join("steammessages_clientserver.proto"),
        proto_dir.join("steammessages_clientserver_userstats.proto"),
        proto_dir.join("steammessages_clientserver_appinfo.proto"),
        proto_dir.join("steammessages_clientserver_friends.proto"),
        proto_dir.join("steammessages_player.steamclient.proto"),
        proto_dir.join("steammessages_friendmessages.steamclient.proto"),
        proto_dir.join("steammessages_contentsystem.steamclient.proto"),
        proto_dir.join("content_manifest.proto"),
    ];

    println!("cargo:rerun-if-changed=build.rs");
    for proto in &protos {
        println!("cargo:rerun-if-changed={}", proto.display());
    }

    let file_descriptors = protox::compile(&protos, [&proto_dir])?;

    let mut config = prost_build::Config::new();
    config.include_file("_includes.rs");
    config.compile_well_known_types();
    config.compile_fds(file_descriptors)?;

    Ok(())
}
