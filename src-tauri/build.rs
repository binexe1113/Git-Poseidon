fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "check_git",
            "open_repository",
            "refresh_repository",
            "get_commit",
            "get_history",
            "get_commit_files",
            "get_file_diff",
            "get_working_changes",
            "get_working_diff",
            "set_repository_trust",
            "stage_file",
            "unstage_file",
            "prepare_branch",
            "execute_branch",
            "set_branch_hook_trust",
            "prepare_commit",
            "create_commit",
            "set_commit_hook_trust",
            "runtime_probe",
        ]),
    ))
    .expect("failed to build Tauri application manifest");
}
