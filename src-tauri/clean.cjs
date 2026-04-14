const fs = require('fs');
let code = fs.readFileSync('src/lib.rs', 'utf-8');

// Remove set_win_click_through function
code = code.replace(/#\[cfg\(target_os = "windows"\)\]\r?\nfn set_win_click_through[\s\S]*?SetWindowLongPtrW\(hwnd, GWL_EXSTYLE, new_ex\);\r?\n    }\r?\n}/g, '');

// Swap to use set_ignore_cursor_events
code = code.replace(/set_win_click_through\(hwnd, !is_hovering\);/g, 'let _ = widget.set_ignore_cursor_events(!is_hovering);');

// Remove WS_EX_TRANSPARENT from Builder
code = code.replace(', WS_EX_TRANSPARENT', '');
code = code.replace(' | WS_EX_TRANSPARENT.0 as isize', '');
code = code.replace('log::info!("Widget HWND configured with WS_EX_NOACTIVATE and WS_EX_TRANSPARENT.");', 'log::info!("Widget HWND configured with WS_EX_NOACTIVATE.");');

// Remove HWND grabs inside the thread because we don't need them
code = code.replace(/    let hwnd_raw: isize = match widget\.hwnd\(\) \{[\s\S]*?return,\r?\n    \};\r?\n/g, '');
code = code.replace(/            use windows::Win32::Foundation::HWND;\r?\n            let hwnd = HWND\(hwnd_raw as \*mut _\);\r?\n/g, '');

fs.writeFileSync('src/lib.rs', code, 'utf-8');
