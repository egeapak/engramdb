# Release notes draft — `[daemon]` moved to the global config

> Draft for the release that moves the shared daemon's settings out of
> project configs.

## Breaking: `[daemon]` is read only from the global config file

The embedding daemon is one process shared by every project and session of a
user. Its settings now come from one place: the `[daemon]` section of the
**global config file**.

| Platform | Global config file |
|----------|--------------------|
| Linux | `~/.config/engramdb/config.toml` (or `$XDG_CONFIG_HOME/engramdb/config.toml`) |
| macOS | `~/Library/Application Support/engramdb/config.toml` |
| Any | `$ENGRAMDB_CONFIG_DIR/config.toml` when `ENGRAMDB_CONFIG_DIR` is set |

A missing file means the defaults. The file holds only `[daemon]`. Model
settings (`[embeddings]`, `[nli]`, `[rerank]`, `[title]`) stay in each
project's `.engramdb/config.toml`.

**What to do:** if a project's `.engramdb/config.toml` has a `[daemon]`
section, move its keys to the global file and delete the section. Until you
do, the project config still loads, but the keys are **ignored**:

- EngramDB logs a warning once per project config, naming each key and the
  global file.
- `engramdb doctor` shows a warning on the project's "Config file" check.

This covers `enabled`, `use_for_cli`, `idle_timeout_secs` and `socket_path`.
`enabled` and `use_for_cli` are now per user, so the daemon is used the same
way in every project. To skip the daemon for one command or session, use
`--in-process` or `ENGRAMDB_IN_PROCESS=1`.

## Why

- **The first spawner decided the idle timeout.** The spawning project passed
  its own `idle_timeout_secs` to the daemon, and every other project's value
  was ignored until the daemon restarted. Each session still computed its
  heartbeat interval from its own project, so a session could ping slower
  than the daemon reaped, and the daemon churned. Now the daemon reads the
  global value itself, and every heartbeat derives from the same value.
- **Two `socket_path` values meant two daemons**, each loading every model.
- **The daemon read a project directory named over its socket** to choose
  models. Clients now send the model settings with each request.

## Other changes

- **Daemon folder.** The socket, the reclaim lock and the daemon log now share
  one folder: the socket's folder. The log moved from
  `<data dir>/logs/daemon.log` to `<daemon folder>/daemon.log`. Under
  `$XDG_RUNTIME_DIR` this folder is cleared on logout.
- **The folder must be private.** On Unix the daemon creates the folder with
  mode `0700`. If it already exists with another owner or a looser mode, the
  daemon refuses to start (models load in-process) and the error names the
  fix. Earlier versions tightened a loose folder silently.
- **New default folder without `$XDG_RUNTIME_DIR`.** The fallback moved from
  `<cache dir>/engramdb/` (shared with the model cache) to
  `<cache dir>/engramdb/daemon/` (macOS: `~/Library/Caches/engramdb/daemon/`).
  A daemon started by an older version at the old path is not contacted; it
  exits on its own after its idle timeout.
- **Daemon protocol 5.** Requests carry the model selection instead of a
  project directory. An older daemon is asked to shut down and replaced; a
  newer one is left alone and the client loads models in-process.
