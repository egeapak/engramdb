"""Invoice PDF rendering (v2: WeasyPrint, enabled by the invoice-pdf-v2 flag)."""
from weasyprint import CSS, HTML
from weasyprint.text.fonts import FontConfiguration

from src import flags

_FONTS = FontConfiguration()
_STYLE = CSS(
    string="@font-face { font-family: Inter; src: url(assets/Inter.woff2); } body { font-family: Inter; }",
    font_config=_FONTS,
)


def render_pdf(html):
    if not flags.is_on("invoice-pdf-v2"):
        raise RuntimeError("the old wkhtmltopdf renderer was removed; enable invoice-pdf-v2")
    return HTML(string=html).write_pdf(stylesheets=[_STYLE], font_config=_FONTS)
