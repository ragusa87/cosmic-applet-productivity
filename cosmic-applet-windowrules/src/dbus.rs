pub async fn show_workspace_overview() -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    conn.call_method(
        Some("com.system76.CosmicWorkspaces"),
        "/com/system76/CosmicWorkspaces",
        Some("com.system76.CosmicWorkspaces"),
        "Show",
        &(),
    )
    .await?;
    Ok(())
}
