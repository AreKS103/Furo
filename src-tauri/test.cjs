const fs = require('fs');
let code = fs.readFileSync('src/lib.rs', 'utf-8');
code = code.replace(
  'fn start_widget_hover_tracker_win(widget: tauri::WebviewWindow) {',
  'fn start_widget_hover_tracker_win(widget: tauri::WebviewWindow) {\nlet _ = widget.set_ignore_cursor_events(true);\n'
);
fs.writeFileSync('src/lib.rs', code, 'utf-8');
