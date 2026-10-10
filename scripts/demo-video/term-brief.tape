# Terminal shot 2: a brief. The width gives 99 columns, where this brief wraps
# on spaces rather than inside a word. Run from the build directory
# (build.sh does): vhs writes PNG frames into ./brief/, and setup.sh is the
# copy of term-setup.sh beside it.
Output brief/
Set Shell "bash"
Set FontFamily "Menlo"
Set FontSize 26
Set LineHeight 1.35
Set Width 1624
Set Height 250
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
Type "prx brief acme/web#1288"
Sleep 450ms
Enter
Sleep 5.4s
