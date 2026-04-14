const fs = require('fs');
let code = fs.readFileSync('src/lib.rs', 'utf-8');
code = code.replace(
  'set_win_click_through(hwnd, !is_hovering);',
  'let _ = widget.set_ignore_cursor_events(!is_hovering);'
);
fs.writeFileSync('src/lib.rs', code, 'utf-8');
