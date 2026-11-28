from fx.total import total


def test_totals_a_basket():
    assert total([{"price": 2, "qty": 3}, {"price": 1, "qty": 1}]) == 7


def test_an_empty_basket_is_zero():
    assert total([]) == 0
