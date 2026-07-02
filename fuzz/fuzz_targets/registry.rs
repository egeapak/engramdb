#![no_main]

use engramdb::storage::registry::{
    collect_descendants, list_children, resolve_root_project_id, Registry,
};
use libfuzzer_sys::fuzz_target;

// registry.json is a user-writable global file read on every command
// (`FileRegistry::load` -> `serde_json::from_str::<Registry>`). This drives
// that exact pub deserialization surface with arbitrary bytes — malformed
// input must always surface as a serde `Err`, never a panic — and then walks
// the parsed data through the pub parent-link graph functions, whose inputs
// (duplicate project IDs, self-parents, cycles, empty strings) all originate
// from that untrusted file. The walks must terminate (cycle-bounded) and stay
// within the registry's size; the filesystem-touching helpers
// (`conflicting_checkout_path`, `FileRegistry` itself) are deliberately not
// exercised here.
fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(registry) = serde_json::from_str::<Registry>(s) else {
        return;
    };

    let n = registry.projects.len();
    for entry in &registry.projects {
        // Must terminate even on parent cycles; the root is echoed input or a
        // project_id present in the registry, so it is never spuriously grown.
        let _root = resolve_root_project_id(&registry, &entry.project_id);

        let children = list_children(&registry, &entry.project_id);
        assert!(
            children.len() <= n,
            "list_children returned more entries than the registry holds"
        );

        // Descendants are deduplicated by ID and exclude the start node, so
        // the walk is bounded by the registry size even with cycles or
        // duplicate project IDs.
        let descendants = collect_descendants(&registry, &entry.project_id);
        assert!(
            descendants.len() <= n,
            "collect_descendants escaped the registry bound"
        );
        assert!(
            !descendants.contains(&entry.project_id),
            "a project must not be its own descendant"
        );
    }

    // An ID absent from the registry must echo back unchanged (the fuzzer
    // can legitimately mint an entry with this exact ID, so guard first).
    const PROBE: &str = "\u{0}no-such-project";
    if !registry.projects.iter().any(|e| e.project_id == PROBE) {
        assert_eq!(resolve_root_project_id(&registry, PROBE), PROBE);
    }
});
