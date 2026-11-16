# -*- coding: utf-8 -*-
#------------------------------------------------------------------
# 시험용 합성 문서 만들기 — tests/samples/ 의 일부
#=> 공개 저장소에 실제 문서를 넣지 않으려고 내용까지 지어낸 시험 문서를 만든다.
#   이 스크립트가 만드는 것: 글자층 PDF(2쪽) · HTML · ZIP 압축(문서 2건 + 안쪽 ZIP 1건)
#   docx · xlsx · pptx · md · txt · 스캔 PDF · 작은 그림은 Struct-DocParse-OCR-rust 의
#   tools/make_samples.py 로 만든 것을 그대로 가져왔다(같은 지어낸 내용).
#   hwp · hwpx · doc · xls · ppt 는 만들 도구가 없어 빠진다 — tests/private/(git 에 안 올라감)에 직접 넣어 시험한다.
#
#   필요: PyMuPDF (pip install pymupdf)
#   사용: python tools/make_samples.py
#------------------------------------------------------------------
import io
import os
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "tests", "samples")

PAGES = [
    ["문서 파서 시험용 PDF", "", "이 쪽은 글자층이 있는 PDF 의 첫째 쪽이다.",
     "본문 추출기는 pdfium 으로 쪽마다 글을 읽는다.", "항목  값  비고", "형식  pdf  시험"],
    ["둘째 쪽", "", "처리 절차와 결과를 적은 쪽이다.", "결과는 쪽 순서대로 이어 붙인다."],
]


#------------------------------------------------------------------
# 글자층 PDF 만들기
#=> PyMuPDF 의 내장 한글 글꼴(korea)로 글을 써서 작은 PDF 를 만든다.
#   글꼴 파일을 통째로 넣지 않으므로 파일이 몇 KB 에 그친다.
#
# -in: path = 만들 파일 경로
#
# -out: 없음
# -out: error = PyMuPDF 가 없으면 ImportError
#------------------------------------------------------------------
def make_text_pdf(path):
    import pymupdf
    doc = pymupdf.open()
    for lines in PAGES:
        page = doc.new_page(width=595, height=842)
        y = 80
        for i, line in enumerate(lines):
            # 첫 줄은 제목처럼 크게, 나머지는 본문 크기
            size = 18 if i == 0 else 11
            if line:
                page.insert_text((60, y), line, fontname="korea", fontsize=size)
            y += size + 10
    doc.save(path, garbage=4, deflate=True)


#------------------------------------------------------------------
# HTML 만들기
#=> 머리말·문단·표·스크립트가 섞인 페이지. 스크립트 글은 본문에 나오면 안 된다.
#
# -in: path = 만들 파일 경로
#
# -out: 없음
# -out: error = 쓰기 실패면 OSError
#------------------------------------------------------------------
def make_html(path):
    body = """<!doctype html>
<html lang="ko"><head><meta charset="utf-8"><title>웹 문서 예시</title>
<style>body { font-family: sans-serif; }</style>
<script>var 숨김 = "이 글은 본문에 나오면 안 된다";</script></head>
<body>
<h1>웹 문서 예시</h1>
<p>이 페이지는 HTML 본문 추출을 시험하려고 지어낸 문서다.</p>
<p>문자 참조도 풀어야 한다: &lt;괄호&gt; &amp; &quot;따옴표&quot; &#xD55C;&#xAE00;</p>
<table><tr><th>항목</th><th>값</th></tr><tr><td>형식</td><td>html</td></tr></table>
</body></html>
"""
    io.open(path, "w", encoding="utf-8", newline="\n").write(body)


#------------------------------------------------------------------
# ZIP 압축 만들기
#=> 압축 펼치기(expand) 시험용. 문서 2건과, 안에 다시 ZIP 이 든 중첩 구조를 만든다.
#    1) 안쪽 ZIP: 평문 1건
#    2) 바깥 ZIP: docx 1건 + md 1건 + 안쪽 ZIP
#
# -in: path = 만들 파일 경로
#
# -out: 없음
# -out: error = 원본 시험 문서가 없으면 OSError
#------------------------------------------------------------------
def make_zip(path):
    inner = io.BytesIO()
    with zipfile.ZipFile(inner, "w", zipfile.ZIP_DEFLATED) as z:
        z.write(os.path.join(OUT, "평문_예시.txt"), "안쪽/평문_예시.txt")
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.write(os.path.join(OUT, "보고서_예시.docx"), "보고서_예시.docx")
        z.write(os.path.join(OUT, "문서_예시.md"), "문서_예시.md")
        z.writestr("안쪽_압축.zip", inner.getvalue())


#------------------------------------------------------------------
# 전부 만들기
#
# -in: 없음
#
# -out: 없음
# -out: error = 위 함수들의 예외 전파
#------------------------------------------------------------------
def main():
    os.makedirs(OUT, exist_ok=True)
    make_text_pdf(os.path.join(OUT, "글자층_예시.pdf"))
    make_html(os.path.join(OUT, "웹문서_예시.html"))
    make_zip(os.path.join(OUT, "묶음_예시.zip"))
    print("만듦:", OUT)


if __name__ == "__main__":
    main()
