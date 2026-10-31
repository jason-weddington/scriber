//! Spoken keyboard shortcuts written as Mac glyphs.

#[cfg(test)]
mod tests {
    use scriber_core::shortcuts::to_glyphs;

    const MUST_CONVERT: [(&str, &str); 35] = [
        (
            "Press command-backslash to toggle the library.",
            "Press ⌘\\ to toggle the library.",
        ),
        (
            "Settings opens with command comma.",
            "Settings opens with ⌘,.",
        ),
        (
            "Use command-slash to comment out the line.",
            "Use ⌘/ to comment out the line.",
        ),
        (
            "Zoom in with command-equals and zoom out with command-minus.",
            "Zoom in with ⌘= and zoom out with ⌘-.",
        ),
        (
            "Hit Command-Shift-P to open the palette.",
            "Hit ⇧⌘P to open the palette.",
        ),
        (
            "Try Option-Command-I for the DevTools.",
            "Try ⌥⌘I for the DevTools.",
        ),
        ("Ctrl Shift Tab goes back a tab.", "⌃⇧⇥ goes back a tab."),
        (
            "The shortcut is Command-K, then Command-S.",
            "The shortcut is ⌘K, then ⌘S.",
        ),
        (
            "Press Shift-Option-Command-V to paste without formatting.",
            "Press ⌥⇧⌘V to paste without formatting.",
        ),
        (
            "Press Ctrl C to stop it, then hit escape.",
            "Press ⌃C to stop it, then hit escape.",
        ),
        ("Command-return sends the message.", "⌘↩ sends the message."),
        ("Shift Enter adds a new line.", "⇧↩ adds a new line."),
        ("Just press Command Comma.", "Just press ⌘,."),
        (
            "Command-period cancels, and command-left-bracket goes back.",
            "⌘. cancels, and ⌘[ goes back.",
        ),
        (
            "Control-Option-Command-F puts it in fullscreen.",
            "⌃⌥⌘F puts it in fullscreen.",
        ),
        (
            "Bold is straightforward, it should just be Command-B and then italics is Command-I.",
            "Bold is straightforward, it should just be ⌘B and then italics is ⌘I.",
        ),
        (
            "Slack also supports Command-Shift-C for adding backticks.",
            "Slack also supports ⇧⌘C for adding backticks.",
        ),
        (
            "The command enter glyph looks better on my work version.",
            "The ⌘↩ glyph looks better on my work version.",
        ),
        (
            "Add command plus comma shortcut to open the settings.",
            "Add ⌘, shortcut to open the settings.",
        ),
        ("Command-plus-comma opens settings.", "⌘, opens settings."),
        (
            "Rebind Command Option L to format the file.",
            "Rebind ⌥⌘L to format the file.",
        ),
        (
            "I keep hitting command-W by accident.",
            "I keep hitting ⌘W by accident.",
        ),
        (
            "Control-A then D detaches the screen session.",
            "⌃A then D detaches the screen session.",
        ),
        (
            "In the terminal, Ctrl-R searches history.",
            "In the terminal, ⌃R searches history.",
        ),
        (
            "Add Command-1 through Command-9 to switch tabs.",
            "Add ⌘1 through ⌘9 to switch tabs.",
        ),
        (
            "Option-Left moves back one word.",
            "⌥← moves back one word.",
        ),
        (
            "Make it Command-Shift-Return instead.",
            "Make it ⇧⌘↩ instead.",
        ),
        (
            "Command-delete should remove the row.",
            "⌘⌫ should remove the row.",
        ),
        (
            "Shift-Tab moves focus backwards.",
            "⇧⇥ moves focus backwards.",
        ),
        ("Command-Space opens Spotlight.", "⌘Space opens Spotlight."),
        (
            "Use Command-Up to jump to the top.",
            "Use ⌘↑ to jump to the top.",
        ),
        (
            "Command-Shift-Period shows hidden files.",
            "⇧⌘. shows hidden files.",
        ),
        (
            "The modal closes on Escape or Command-Period.",
            "The modal closes on Escape or ⌘..",
        ),
        (
            "Alt-F4 does nothing on a Mac.",
            "⌥F4 does nothing on a Mac.",
        ),
        ("Bind it to Command-Control-Space.", "Bind it to ⌃⌘Space."),
    ];

    const MUST_STAY: [&str; 37] = [
        "Run that command, then commit the change.",
        "The command palette should open at the top of the window.",
        "Open the slash command menu.",
        "The command line tool takes a dash dash verbose option.",
        "Option two is better, but we lose some control.",
        "The control plane dispatches a command to each worker.",
        "Give me the command, and then put a comma after the name.",
        "Escape the backslash in that regex.",
        "Shift the option menu to the left of the control panel.",
        "Enter the command, then return to the main page.",
        "The command key on this keyboard is sticking.",
        "Use the control key instead of the command key.",
        "Option-click the toolbar button.",
        "Shift-click to select a range.",
        "Hold shift and drag the corner.",
        "There was a big shift in control after the command changed.",
        "Option B has a plus and a minus.",
        "The command, shift the window and tab order, is fine.",
        "The meeting is at 9.43 a.m. and qwen 3.8 is the model.",
        "Press the space bar and hit enter.",
        "Each command takes an option and a slash separated path.",
        "Shift left by two bits.",
        "Option 1 is cheaper than option 2.",
        "The team needs to shift down a gear.",
        "In command mode the cursor moves.",
        "We need better command and control.",
        "Shift up to the next tier.",
        "Press command, then wait for the prompt.",
        "The shift key and the option key do different things.",
        "The command returned an error.",
        "Add a control to the form for the option.",
        "Command S3 sync copies the files.",
        "Alt text is missing on that image.",
        "Set the option to down.",
        "Control flow should return early.",
        "Delete the command and tab back to the editor.",
        "Command-plus-comma-open-settings",
    ];

    const MISFIRES: [(&str, &str); 4] = [
        (
            "We should shift enter key handling into the editor.",
            "We should ⇧↩ key handling into the editor.",
        ),
        (
            "Control A and control B are the two groups in the experiment.",
            "⌃A and ⌃B are the two groups in the experiment.",
        ),
        (
            "Use the option return value instead of unwrap.",
            "Use the ⌥↩ value instead of unwrap.",
        ),
        (
            "This is a Shift Enter style of handler.",
            "This is a ⇧↩ style of handler.",
        ),
    ];

    const KEYS: [(&str, &str); 35] = [
        ("forward slash", "/"),
        ("right bracket", "]"),
        ("close bracket", "]"),
        ("left bracket", "["),
        ("open bracket", "["),
        ("backslash", "\\"),
        ("semicolon", ";"),
        ("backspace", "⌫"),
        ("backtick", "`"),
        ("period", "."),
        ("equals", "="),
        ("hyphen", "-"),
        ("return", "↩"),
        ("escape", "⎋"),
        ("delete", "⌫"),
        ("comma", ","),
        ("slash", "/"),
        ("equal", "="),
        ("minus", "-"),
        ("quote", "'"),
        ("enter", "↩"),
        ("space", "Space"),
        ("right", "→"),
        ("dash", "-"),
        ("down", "↓"),
        ("left", "←"),
        ("dot", "."),
        ("tab", "⇥"),
        ("esc", "⎋"),
        ("up", "↑"),
        ("forward-slash", "/"),
        ("left-bracket", "["),
        ("right-bracket", "]"),
        ("open-bracket", "["),
        ("close-bracket", "]"),
    ];

    const CONVERT: &[(&str, &str)] = &[
        ("Command-FORWARD slash", "⌘/"),
        ("Cmd-K", "⌘K"),
        ("Control-K", "⌃K"),
        ("Ctrl-K", "⌃K"),
        ("Option-K", "⌥K"),
        ("Alt-Shift-K", "⌥⇧K"),
        ("Shift-K", "⇧K"),
        ("Shift-A", "⇧A"),
        ("Option-B", "⌥B"),
        ("Ctrl Option Shift Command K", "⌃⌥⇧⌘K"),
        ("Command-Command-K", "⌘K"),
        ("command k", "⌘K"),
        ("command-k", "⌘K"),
        ("command+k", "⌘K"),
        ("command + k", "⌘K"),
        ("command-plus-k", "⌘K"),
        ("command plus k", "⌘K"),
        ("commandplus-k", "⌘K"),
        ("cmd plus k", "⌘K"),
        ("Command plus Shift plus P", "⇧⌘P"),
        ("Command + Shift + P", "⇧⌘P"),
        ("Command +\nk", "⌘K"),
        ("(command-k)", "(⌘K)"),
        ("command-k.", "⌘K."),
        ("command-k ok", "⌘K ok"),
        ("ok command-k", "ok ⌘K"),
        ("COMMAND-K", "⌘K"),
        ("Ctrl-c", "⌃C"),
        ("command-forward-slash", "⌘/"),
        ("Command-F1", "⌘F1"),
        ("Command-F19", "⌘F19"),
        ("command f19", "⌘F19"),
        ("Command-F0", "⌘F0"),
        ("Command-F12 x", "⌘F12 x"),
        ("Command-1", "⌘1"),
        ("Option-1", "⌥1"),
        ("Shift Tab", "⇧⇥"),
        ("Option Return", "⌥↩"),
        ("Shift Option Left", "⌥⇧←"),
        ("Option-b", "⌥B"),
        ("Shift left-bracket", "⇧["),
        ("shift-plus-a", "⇧A"),
        ("Shift-Shift-a", "⇧A"),
    ];

    const STAY: &[&str] = &[
        "command plus-k",
        "Command\nK",
        "xcommand-k",
        "_command-k",
        "-command-k",
        "écommand-k",
        "command-kx",
        "command-k_",
        "command-k-",
        "Command-F20",
        "Command-10",
        "Option 1",
        "shift left",
        "Option b",
        "Shift a",
        "Shift Shift a",
        "Command-Shift-click",
        "command shift",
    ];

    #[test]
    fn the_corpus_converts() {
        assert_eq!(MUST_CONVERT.len(), 35, "the whole corpus");
        for (input, want) in MUST_CONVERT {
            assert_eq!(to_glyphs(input), want, "{input}");
        }
    }

    #[test]
    fn the_corpus_stays() {
        assert_eq!(MUST_STAY.len(), 37, "the whole corpus");
        for s in MUST_STAY {
            assert_eq!(to_glyphs(s), s, "{s}");
        }
    }

    #[test]
    fn the_accepted_misfires_are_pinned() {
        assert_eq!(MISFIRES.len(), 4, "the whole list");
        for (input, want) in MISFIRES {
            assert_eq!(to_glyphs(input), want, "{input}");
        }
    }

    #[test]
    fn every_key_word_converts() {
        assert_eq!(KEYS.len(), 35, "all keys");
        for (word, out) in KEYS {
            assert_eq!(
                to_glyphs(&format!("Command-{word}")),
                format!("⌘{out}"),
                "{word}"
            );
        }
    }

    #[test]
    fn modifiers_separators_boundaries_fkeys_and_evidence() {
        for (input, want) in CONVERT {
            assert_eq!(to_glyphs(input), *want, "{input:?}");
        }
        for s in STAY {
            assert_eq!(to_glyphs(s), *s, "{s:?}");
        }
    }

    #[test]
    fn converting_twice_changes_nothing() {
        let all = MUST_CONVERT
            .iter()
            .flat_map(|(a, _)| [*a])
            .chain(MUST_STAY)
            .chain(MISFIRES.iter().map(|(a, _)| *a))
            .chain(CONVERT.iter().map(|(a, _)| *a))
            .chain(STAY.iter().copied())
            .chain(KEYS.iter().map(|(w, _)| *w));
        for s in all {
            let once = to_glyphs(s);
            assert_eq!(to_glyphs(&once), once, "{s:?}");
        }
        for (word, _) in KEYS {
            let once = to_glyphs(&format!("Command-{word}"));
            assert_eq!(to_glyphs(&once), once, "{word}");
        }
    }

    #[test]
    fn edge_cases() {
        assert_eq!(to_glyphs(""), "", "empty");
        assert_eq!(to_glyphs("a\n\ncommand-k\nb"), "a\n\n⌘K\nb", "newlines");
        assert_eq!(
            to_glyphs("command-backslash")
                .chars()
                .filter(|c| *c == '\\')
                .count(),
            1,
            "one backslash"
        );
    }
}
