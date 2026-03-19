fn main() {
    embuild::espidf::sysenv::output();
    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer);
    slint_build::compile_with_config("ui/app.slint", config)
        .expect("Slint build failed");
}
