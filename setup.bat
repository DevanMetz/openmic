@echo off
setlocal
cd /d "%~dp0"
where python >nul 2>&1 || (
  echo Python was not found. Install Python 3 and try again.
  exit /b 1
)
python -m venv .venv || exit /b 1
.venv\Scripts\python.exe -m pip install --upgrade pip || exit /b 1
.venv\Scripts\python.exe -m pip install -r requirements.txt || exit /b 1
echo.
echo Setup complete. Run run.bat to start OpenMic.
