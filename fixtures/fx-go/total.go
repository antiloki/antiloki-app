// Package fxgo is small on purpose: this fixture exists to be discovered, not to be impressive.
package fxgo

// Item is a line in a basket.
type Item struct {
	Price int
	Qty   int
}

// Total sums a basket.
func Total(items []Item) int {
	sum := 0
	for _, i := range items {
		sum += i.Price * i.Qty
	}
	return sum
}
