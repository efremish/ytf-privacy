@echo off
chcp 65001 >nul
title ytf - YouTube pipeline
cd /d "%~dp0"

:menu
cls
echo ============================================================
echo   ytf - avtozalive type beat na YouTube
echo ============================================================
echo.
echo   1. Proverka okruzheniya (doctor)
echo   2. Pokazat kanaly (library)
echo   3. Proverit tokeny (tokens)
echo   4. Podklyuchit akkount kanala (auth)
echo   5. Opublikovat video (publish)
echo   6. Opublikovat VSE kanaly (publish)
echo   7. Render bez zagruzki (test)
echo   8. Zalit shortsy (Shorts\)
echo   9. Veb-panel (otkroetsya v brauzere)
echo  10. Vyhod
echo.
set /p choice="Vyberi cifru i nazhmi Enter: "

if "%choice%"=="1" goto doctor
if "%choice%"=="2" goto library
if "%choice%"=="3" goto tokens
if "%choice%"=="4" goto auth
if "%choice%"=="5" goto publish_one
if "%choice%"=="6" goto publish_all
if "%choice%"=="7" goto dry
if "%choice%"=="8" goto shorts
if "%choice%"=="9" goto web
if "%choice%"=="10" exit
goto menu

:doctor
cls
.\ytf.exe doctor
echo.
pause
goto menu

:library
cls
.\ytf.exe library
echo.
pause
goto menu

:tokens
cls
.\ytf.exe tokens
echo.
pause
goto menu

:auth
cls
echo Kanaly, kotorye est v People\:
echo.
.\ytf.exe library | findstr /C:"[+]" /C:"[!]"
echo.
echo Vpishi imya PAPKI kanala (naprimer: my-channel)
echo Ono vidno v skobkah v stroke vyshe, posle imeni cheloveka.
echo.
set /p folder="Imya papki kanala: "
if "%folder%"=="" goto auth
set /p title="Nazvanie kanala na YouTube (mozhno Enter): "
if "%title%"=="" (
  .\ytf.exe auth "%folder%"
) else (
  .\ytf.exe auth "%folder%" --title "%title%"
)
echo.
pause
goto menu

:publish_one
cls
echo Kanaly, kotorye gotovy k publikacii:
echo.
.\ytf.exe library | findstr /C:"[+]"
echo.
set /p folder="Imya papki kanala (ili Enter dlya vseh): "
if "%folder%"=="" goto publish_all
.\ytf.exe publish "%folder%"
echo.
pause
goto menu

:publish_all
cls
.\ytf.exe publish
echo.
pause
goto menu

:dry
cls
echo Kanaly:
echo.
.\ytf.exe library | findstr /C:"[+]"
echo.
set /p folder="Imya papki kanala: "
if "%folder%"=="" goto dry
.\ytf.exe publish "%folder%" --no-upload
echo.
pause
goto menu

:shorts
cls
echo Shortsy: kladi gotovye .mp4 v papku Shorts (imya papki) i zhalis "8".
echo Po umolchaniyu beret 1 video na kanal i stavit otlozhku.
echo.
set /p mode="1 - po 1 na kanal (s otlozhkoy), 2 - vsya ochered, 3 - tolko proverka: "
echo.
set /p folder="Imya papki kanala (ili Enter dlya vseh): "
if "%mode%"=="3" (
  if "%folder%"=="" (
    .\ytf.exe shorts --no-upload
  ) else (
    .\ytf.exe shorts "%folder%" --no-upload
  )
) else if "%mode%"=="2" (
  if "%folder%"=="" (
    .\ytf.exe shorts --all
  ) else (
    .\ytf.exe shorts "%folder%" --all
  )
) else (
  if "%folder%"=="" (
    .\ytf.exe shorts
  ) else (
    .\ytf.exe shorts "%folder%"
  )
)
echo.
pause
goto menu

:web
cls
start "" http://localhost:3000
.\ytf.exe web --port 3000
pause
goto menu
