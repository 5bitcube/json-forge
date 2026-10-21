@echo off
cargo build --release
if %errorlevel%==0 (
    copy /Y target\release\json-forge.exe json-forge.exe >nul
    copy /Y target\release\spellchecker.exe spellchecker.exe >nul
    echo Build successful: json-forge.exe, spellchecker.exe
) else (
    echo Build failed!
)
pause
