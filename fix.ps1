(Get-Content src/components/Dashboard.tsx) -replace 'import \{ getCurrentWindow \} from "@tauri-apps/api/window";', '' | Set-Content src/components/Dashboard.tsx
