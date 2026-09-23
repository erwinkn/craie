# Pinned test fonts

Fonts for deterministic text tests and the E01 comparison (owned
paragraph layout versus Parley). Both paths read exactly these files, so
results do not depend on the machine's installed fonts. Production text
uses the platform's font source (fontique on desktop).

All files are from the Noto project under the SIL Open Font License 1.1
(`OFL.txt`), fetched 2026-09-23:

| File | Source |
|------|--------|
| NotoSans-Regular.ttf, -Bold.ttf, -Italic.ttf | notofonts.github.io `fonts/NotoSans/hinted/ttf/` |
| NotoSansArabic-Regular.ttf | `fonts/NotoSansArabic/hinted/ttf/` |
| NotoSansHebrew-Regular.ttf | `fonts/NotoSansHebrew/hinted/ttf/` |
| NotoSansDevanagari-Regular.ttf | `fonts/NotoSansDevanagari/hinted/ttf/` |
| NotoSansSymbols2-Regular.ttf | `fonts/NotoSansSymbols2/hinted/ttf/` (has U+2715 ✕) |
| NotoSansJP-Subset-Regular.otf | noto-cjk `Sans/SubsetOTF/JP/NotoSansJP-Regular.otf`, subset below |

The Japanese face is subset to keep the repository small (234 KB from
4.5 MB): CJK punctuation, hiragana, katakana, half- and full-width
forms, and the kanji the test corpus uses.

```
pyftsubset NotoSansJP-Regular.otf \
  --unicodes="U+3000-303F,U+3040-309F,U+30A0-30FF,U+FF00-FFEF" \
  --text="日本語文章表示東京大阪漢字改行組版列処理段落幅一二三四五六七八九十百千万円年月時分人子女男山川田中上下左右小本書読見聞話言学生先会社国家電車雨花雪天気今週末新旧高低長短多少明暗白黒赤青緑色音楽映画店食物飲水火木金土曜朝昼夜春夏秋冬海空道駅前後内外入出来行帰休働使作持待開閉始終知思考教習練問題答" \
  --layout-features='*' --output-file=NotoSansJP-Subset-Regular.otf
```

A test text that needs other kanji must add them to the subset here.
