# WH3 Mod Manager Rust: Windows Alpha Verification

This build is ready for a focused Windows verification pass. Use a disposable
preset or a mod selection you can reconstruct. Keep Steam running and allow
Warhammer III to finish any pending update before starting.

## 1. Install or extract

Test one distribution first, then the other if the first succeeds:

1. Extract the portable zip to a normal writable folder, or run the installer.
2. Start `wh3mm-dioxus.exe`.
3. Confirm the window title is **WH3 Mod Manager** and no extra native menu bar
   appears above the app UI.

If Windows SmartScreen appears, record the exact warning. Do not disable
system-wide security controls.

## 2. Check packaged prerequisites

Open **Checks** from the top bar or Play & Tools rail.

- Schema, Steam helper, and Steam DLL should report `READY`.
- Select the Warhammer III folder if it is not already restored.
- The WH3 folder should report `READY`.
- Workshop may report `CHECK` when there are no subscribed Workshop files yet.

Stop here and collect diagnostics if Schema, Steam helper, or Steam DLL reports
`ERROR`.

## 3. Verify Steam integration

Open **Steam** and keep the backend set to `native`.

1. Run **Probe** and confirm the native backend and `steam_api64.dll` are
   available.
2. Run **Refresh** and confirm subscribed Workshop IDs and mod metadata load.
3. Open **Workshop** and run **Check updates**.
4. Use subscribe/download/unsubscribe/resubscribe only with IDs you are
   comfortable changing. Commands are deduplicated, throttled, and logged.

If Steam reports rate limiting or repeated failures, stop issuing commands and
include the helper command log with the report.

## 4. Verify the mod archive

1. Load the WH3 game folder.
2. Confirm local, data/modding, and Workshop packs appear without duplicates.
3. Search and filter the archive; the `Ord` column must continue to show the
   real load order rather than renumbering visible rows.
4. Open **Mod Settings** from a row and test Previous/Next, enable/disable,
   move, lock, hide, and category assignment.
5. Save a collection/preset, restart the app, and confirm enablement and order
   are restored.
6. Run **Compatibility** for the enabled set and record any pack read errors.

## 5. Verify launch preparation and WH3 start

Open **Launch Options**.

1. Use **Preview** and confirm the enabled pack order matches the archive.
2. Use **Prepare files** and inspect `used_mods.txt` in the WH3 folder. If the
   primary file cannot be written, confirm the app reports its `my_mods.txt`
   fallback.
3. Start with optional generated-pack features disabled and click **PLAY GAME**.
4. Confirm Warhammer III starts and loads the selected packs in the expected
   order.
5. Repeat with the launch options you actually use. Treat Make Units Generals
   and imported pack-data overwrites as separate smoke cases.

## 6. Collect diagnostics

Open **Settings → Diagnostics**:

1. Click **Write snapshot** after the failure or completed test.
2. Click **Open folder**.
3. Keep the newest `wh3mm-diagnostic-*.txt`, `wh3mm-dioxus.log`,
   `wh3mm-crash.log` (when present), and
   `wh3mm-steam-helper-commands.jsonl` files together.

When reporting a problem, include the distribution type (installer or zip),
the failed step, the visible error, and those diagnostic files. Do not include
private save files or unrelated Steam account data.
