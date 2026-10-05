"""Plain-text customer statements."""


def format_cents(cents):
    return f"{cents // 100}.{cents % 100:02d}"


def render_statement(customer_id, invoices):
    lines = [f"Statement for {customer_id}"]
    total = 0
    for inv in invoices:
        lines.append(f"  {inv['id']:<12} {format_cents(inv['total_cents']):>12}")
        total += inv["total_cents"]
    lines.append(f"  {'TOTAL':<12} {format_cents(total):>12}")
    return "\n".join(lines) + "\n"
