#!/bin/bash
# Двойной клик — откроет терминал в папке программы
cd "$(dirname "$0")"
echo "ytf — YouTube pipeline. Команды: doctor, library, tokens, auth, publish, web"
exec bash
