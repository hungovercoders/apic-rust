//go:build ignore

// Regenerates style_golden.json: every case in the file is rendered again
// with lipgloss, x/ansi and termenv, the libraries whose output the style
// module reproduces byte for byte. Run it inside a Go module that has
// github.com/charmbracelet/lipgloss v1.1.0 and
// github.com/charmbracelet/x/ansi v0.11.9:
//
//	go run golden.go style_golden.json style_golden.json
package main

import (
	"encoding/hex"
	"encoding/json"
	"os"

	"github.com/charmbracelet/lipgloss"
	"github.com/charmbracelet/x/ansi"
	"github.com/charmbracelet/x/ansi/parser"
	"github.com/muesli/termenv"
)

type styleSpec struct {
	Bold, Italic, Underline, Strikethrough, Reverse, Blink, Faint *bool
	UnderlineSpaces                                               *bool   `json:"underline_spaces"`
	StrikethroughSpaces                                           *bool   `json:"strikethrough_spaces"`
	ColorWhitespace                                               *bool   `json:"color_whitespace"`
	Inline                                                        *bool   `json:"inline"`
	Fg, Bg                                                        *string
	MarginBg                                                      *string `json:"margin_bg"`
	BorderFg                                                      *string `json:"border_fg"`
	BorderBg                                                      *string `json:"border_bg"`
	FgAdaptive                                                    []string `json:"fg_adaptive"`
	FgComplete                                                    []string `json:"fg_complete"`
	Dark                                                          *bool
	Width, Height                                                 *int
	MaxWidth                                                      *int `json:"max_width"`
	MaxHeight                                                     *int `json:"max_height"`
	TabWidth                                                      *int `json:"tab_width"`
	AlignH                                                        *float64 `json:"align_h"`
	AlignV                                                        *float64 `json:"align_v"`
	Padding, Margin                                               []int
	Border                                                        *string
	BorderSides                                                   []bool   `json:"border_sides"`
	BorderStyleOnly                                               bool     `json:"border_style_only"`
	BorderTop                                                     *bool    `json:"border_top"`
	BorderRight                                                   *bool    `json:"border_right"`
	BorderBottom                                                  *bool    `json:"border_bottom"`
	BorderLeft                                                    *bool    `json:"border_left"`
	BorderFgSides                                                 []string `json:"border_fg_sides"`
}

type styleCase struct {
	Style json.RawMessage   `json:"style"`
	Input string            `json:"input"`
	Out   map[string]string `json:"out"`
}

type fnCase struct {
	Fn       string   `json:"fn"`
	S        *string  `json:"s,omitempty"`
	N        *int     `json:"n,omitempty"`
	Tail     *string  `json:"tail,omitempty"`
	L        *int     `json:"l,omitempty"`
	R        *int     `json:"r,omitempty"`
	Preserve *bool    `json:"preserve,omitempty"`
	Bp       *string  `json:"bp,omitempty"`
	Pos      *float64 `json:"pos,omitempty"`
	Blocks   []string `json:"blocks,omitempty"`
	W        *int     `json:"w,omitempty"`
	H        *int     `json:"h,omitempty"`
	Hp       *float64 `json:"hp,omitempty"`
	Vp       *float64 `json:"vp,omitempty"`
	Out      any      `json:"out"`
}

type file struct {
	ParserTable string      `json:"parser_table"`
	Styles      []styleCase `json:"styles"`
	Fns         []fnCase    `json:"fns"`
}

var borders = map[string]lipgloss.Border{
	"normal": lipgloss.NormalBorder(), "rounded": lipgloss.RoundedBorder(), "thick": lipgloss.ThickBorder(),
	"double": lipgloss.DoubleBorder(), "hidden": lipgloss.HiddenBorder(), "block": lipgloss.BlockBorder(),
	"outer_half": lipgloss.OuterHalfBlockBorder(), "inner_half": lipgloss.InnerHalfBlockBorder(),
	"markdown": lipgloss.MarkdownBorder(), "ascii": lipgloss.ASCIIBorder(),
}

func build(sp styleSpec) lipgloss.Style {
	s := lipgloss.NewStyle()
	b := func(v *bool, set func(bool) lipgloss.Style) {
		if v != nil {
			s = set(*v)
		}
	}
	b(sp.Bold, func(v bool) lipgloss.Style { return s.Bold(v) })
	b(sp.Italic, func(v bool) lipgloss.Style { return s.Italic(v) })
	b(sp.Underline, func(v bool) lipgloss.Style { return s.Underline(v) })
	b(sp.Strikethrough, func(v bool) lipgloss.Style { return s.Strikethrough(v) })
	b(sp.Reverse, func(v bool) lipgloss.Style { return s.Reverse(v) })
	b(sp.Blink, func(v bool) lipgloss.Style { return s.Blink(v) })
	b(sp.Faint, func(v bool) lipgloss.Style { return s.Faint(v) })
	b(sp.UnderlineSpaces, func(v bool) lipgloss.Style { return s.UnderlineSpaces(v) })
	b(sp.StrikethroughSpaces, func(v bool) lipgloss.Style { return s.StrikethroughSpaces(v) })
	b(sp.ColorWhitespace, func(v bool) lipgloss.Style { return s.ColorWhitespace(v) })
	b(sp.Inline, func(v bool) lipgloss.Style { return s.Inline(v) })
	if sp.Fg != nil {
		s = s.Foreground(lipgloss.Color(*sp.Fg))
	}
	if sp.Bg != nil {
		s = s.Background(lipgloss.Color(*sp.Bg))
	}
	if sp.FgAdaptive != nil {
		s = s.Foreground(lipgloss.AdaptiveColor{Light: sp.FgAdaptive[0], Dark: sp.FgAdaptive[1]})
	}
	if sp.FgComplete != nil {
		s = s.Foreground(lipgloss.CompleteColor{TrueColor: sp.FgComplete[0], ANSI256: sp.FgComplete[1], ANSI: sp.FgComplete[2]})
	}
	if sp.MarginBg != nil {
		s = s.MarginBackground(lipgloss.Color(*sp.MarginBg))
	}
	if sp.Width != nil {
		s = s.Width(*sp.Width)
	}
	if sp.Height != nil {
		s = s.Height(*sp.Height)
	}
	if sp.MaxWidth != nil {
		s = s.MaxWidth(*sp.MaxWidth)
	}
	if sp.MaxHeight != nil {
		s = s.MaxHeight(*sp.MaxHeight)
	}
	if sp.TabWidth != nil {
		s = s.TabWidth(*sp.TabWidth)
	}
	if sp.AlignH != nil {
		s = s.AlignHorizontal(lipgloss.Position(*sp.AlignH))
	}
	if sp.AlignV != nil {
		s = s.AlignVertical(lipgloss.Position(*sp.AlignV))
	}
	if sp.Padding != nil {
		s = s.Padding(sp.Padding...)
	}
	if sp.Margin != nil {
		s = s.Margin(sp.Margin...)
	}
	if sp.Border != nil {
		if sp.BorderStyleOnly {
			s = s.BorderStyle(borders[*sp.Border])
		} else {
			s = s.Border(borders[*sp.Border], sp.BorderSides...)
		}
	}
	b(sp.BorderTop, func(v bool) lipgloss.Style { return s.BorderTop(v) })
	b(sp.BorderRight, func(v bool) lipgloss.Style { return s.BorderRight(v) })
	b(sp.BorderBottom, func(v bool) lipgloss.Style { return s.BorderBottom(v) })
	b(sp.BorderLeft, func(v bool) lipgloss.Style { return s.BorderLeft(v) })
	if sp.BorderFg != nil {
		s = s.BorderForeground(lipgloss.Color(*sp.BorderFg))
	}
	if sp.BorderBg != nil {
		s = s.BorderBackground(lipgloss.Color(*sp.BorderBg))
	}
	if sp.BorderFgSides != nil {
		var cs []lipgloss.TerminalColor
		for _, c := range sp.BorderFgSides {
			cs = append(cs, lipgloss.Color(c))
		}
		s = s.BorderForeground(cs...)
	}
	return s
}

func run(c fnCase) any {
	str := func(p *string) string {
		if p == nil {
			return ""
		}
		return *p
	}
	switch c.Fn {
	case "string_width":
		return ansi.StringWidth(*c.S)
	case "strip":
		return ansi.Strip(*c.S)
	case "width":
		return lipgloss.Width(*c.S)
	case "height":
		return lipgloss.Height(*c.S)
	case "truncate":
		return ansi.Truncate(*c.S, *c.N, str(c.Tail))
	case "truncate_left":
		return ansi.TruncateLeft(*c.S, *c.N, str(c.Tail))
	case "cut":
		return ansi.Cut(*c.S, *c.L, *c.R)
	case "hardwrap":
		return ansi.Hardwrap(*c.S, *c.N, *c.Preserve)
	case "wordwrap":
		return ansi.Wordwrap(*c.S, *c.N, str(c.Bp))
	case "wrap":
		return ansi.Wrap(*c.S, *c.N, str(c.Bp))
	case "join_horizontal":
		return lipgloss.JoinHorizontal(lipgloss.Position(*c.Pos), c.Blocks...)
	case "join_vertical":
		return lipgloss.JoinVertical(lipgloss.Position(*c.Pos), c.Blocks...)
	case "place_horizontal":
		return lipgloss.PlaceHorizontal(*c.N, lipgloss.Position(*c.Pos), *c.S)
	case "place_vertical":
		return lipgloss.PlaceVertical(*c.N, lipgloss.Position(*c.Pos), *c.S)
	case "place":
		return lipgloss.Place(*c.W, *c.H, lipgloss.Position(*c.Hp), lipgloss.Position(*c.Vp), *c.S)
	}
	panic("unknown fn " + c.Fn)
}

func main() {
	data, err := os.ReadFile(os.Args[1])
	if err != nil {
		panic(err)
	}
	var f file
	if err := json.Unmarshal(data, &f); err != nil {
		panic(err)
	}
	profiles := map[string]termenv.Profile{"truecolor": termenv.TrueColor, "ansi256": termenv.ANSI256, "ansi": termenv.ANSI, "ascii": termenv.Ascii}
	for i := range f.Styles {
		var sp styleSpec
		if err := json.Unmarshal(f.Styles[i].Style, &sp); err != nil {
			panic(err)
		}
		lipgloss.SetHasDarkBackground(sp.Dark == nil || *sp.Dark)
		f.Styles[i].Out = map[string]string{}
		for name, p := range profiles {
			lipgloss.SetColorProfile(p)
			f.Styles[i].Out[name] = build(sp).Render(f.Styles[i].Input)
		}
	}
	lipgloss.SetColorProfile(termenv.Ascii)
	for i := range f.Fns {
		f.Fns[i].Out = run(f.Fns[i])
	}
	f.ParserTable = hex.EncodeToString(parser.Table)
	out, err := os.Create(os.Args[2])
	if err != nil {
		panic(err)
	}
	enc := json.NewEncoder(out)
	enc.SetEscapeHTML(false)
	enc.SetIndent("", " ")
	if err := enc.Encode(f); err != nil {
		panic(err)
	}
	if err := out.Close(); err != nil {
		panic(err)
	}
}
