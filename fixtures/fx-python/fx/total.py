def total(items):
    """Sum a basket. Small on purpose — this fixture exists to be discovered."""
    return sum(i["price"] * i["qty"] for i in items)
