# Terminal shot 1: prx status. Run from the build directory
# (build.sh does): vhs writes PNG frames into ./status/, and setup.sh is the
# copy of term-setup.sh beside it.
Output status/
Set Shell "bash"
Set FontFamily "Menlo"
Set FontSize 20
Set LineHeight 1.35
Set Width 1700
Set Height 660
Set Padding 0
Set Framerate 30
Set TypingSpeed 55ms
Set CursorBlink false
Set Theme { "name": "prx", "black": "#18181B", "red": "#FF7B7B", "green": "#5FD38D", "yellow": "#F2C063", "blue": "#4C96FF", "magenta": "#B49CFF", "cyan": "#6FD3E8", "white": "#D4D4D8", "brightBlack": "#71717A", "brightRed": "#FF9B9B", "brightGreen": "#86E3A9", "brightYellow": "#F7D58B", "brightBlue": "#7DB2FF", "brightMagenta": "#CBB9FF", "brightCyan": "#9BE3F2", "brightWhite": "#F4F4F5", "background": "#15161A", "foreground": "#F4F4F5", "selection": "#2F7BF5", "cursor": "#4C96FF" }

Hide
Type "source setup.sh"
Enter
Sleep 800ms
Show

Sleep 1.2s
Type "prx status"
Sleep 450ms
Enter
Sleep 6.2s
