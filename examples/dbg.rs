fn main() {
    let line = r#"{"WorkspacesChanged":{"workspaces":[{"id":1,"idx":1,"name":null,"output":"eDP-1","is_urgent":false,"is_active":false,"is_focused":false,"active_window_id":6}]}}"#;
    let r = ocm_desktop_niri::debug_parse(line);
    println!("{:?}", r.map(|e| format!("{:?}", e)));
}
