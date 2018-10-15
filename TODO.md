 * case insensitivity. rupa deals with this by treating
   lower-case inputs as a request for case-insensitive matching.
   The regex engine supports it via. pcre syntax (`(?i)`), and
   directly as a builder option that we could provide.

   Additionally, case sensitive matches are considered,
   then case-insensitive matches.

 * echo (`-e` flag)

 * remove (`-x` flag)

 * subcommands are.. uh.. ambiguous with searching
