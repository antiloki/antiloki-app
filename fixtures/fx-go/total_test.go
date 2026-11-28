package fxgo

import "testing"

func TestTotal(t *testing.T) {
	cases := []struct {
		name string
		in   []Item
		want int
	}{
		{"a basket", []Item{{2, 3}, {1, 1}}, 7},
		{"an empty basket", nil, 0},
	}
	for _, c := range cases {
		if got := Total(c.in); got != c.want {
			t.Errorf("%s: got %d, want %d", c.name, got, c.want)
		}
	}
}
