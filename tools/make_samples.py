# -*- coding: utf-8 -*-
#------------------------------------------------------------------
# 시험용 합성 문서 만들기 — tests/samples/ 의 일부
#=> 공개 저장소에 실제 문서를 넣지 않으려고 내용까지 지어낸 시험 문서를 만든다.
#   이 스크립트가 만드는 것: 글자층 PDF(2쪽) · HTML · ZIP 압축(문서 2건 + 안쪽 ZIP 1건) ·
#   본문 밖 글(0.2.0 extra_text) 시험용 주석 PDF · 메모 docx · SmartArt pptx/docx
#   docx · xlsx · pptx · md · txt · 스캔 PDF · 작은 그림은 Struct-DocParse-OCR-rust 의
#   tools/make_samples.py 로 만든 것을 그대로 가져왔다(같은 지어낸 내용).
#   hwp · hwpx · doc · xls · ppt 는 만들 도구가 없어 빠진다 — tests/private/(git 에 안 올라감)에 직접 넣어 시험한다.
#
#   필요: PyMuPDF · python-docx(1.2 이상) · python-pptx
#   사용: python tools/make_samples.py [--force]
#         이미 있는 파일은 건너뛴다(zip 시각 등으로 바이트가 바뀌어 git 에 잡히지 않게). --force 면 다시 만든다.
#------------------------------------------------------------------
import io
import os
import sys
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
# 주석 PDF 만들기 — 본문 밖 글(extra_text) 시험용
#=> 그림 한 장으로 덮은 3쪽(스캔 쪽 꼴)에 여러 종류의 주석을 얹는다. 쪽 본문 글자는 없다.
#    1쪽: 글상자 2개(읽힘) · 숨긴 글상자 1개(읽힘 — 사이냅과 같게) · 쪽지 1개(안 읽힘)
#    2쪽: 글 서식 칸 1개(값 읽힘) · 체크 상자 1개(안 읽힘)
#    3쪽: 도장 1개(글 없음)
#
# -in: path = 만들 파일 경로
#
# -out: 없음
# -out: error = PyMuPDF 가 없으면 ImportError
#------------------------------------------------------------------
def make_annot_pdf(path):
    import pymupdf
    doc = pymupdf.open()
    # 쪽을 덮을 회색 그림(PNG) — 스캔 쪽으로 판정되게
    pix = pymupdf.Pixmap(pymupdf.csRGB, pymupdf.IRect(0, 0, 60, 80), False)
    pix.set_rect(pix.irect, (220, 220, 220))
    png = pix.tobytes("png")
    for _ in range(3):
        pg = doc.new_page(width=595, height=842)
        pg.insert_image(pg.rect, stream=png)
    p1, p2, p3 = doc[0], doc[1], doc[2]
    for i, t in enumerate(["(2026년 시험기관 자료)", "주석으로 덧붙인 설명 글"]):
        a = p1.add_freetext_annot(pymupdf.Rect(60, 100 + i * 50, 420, 130 + i * 50), t,
                                  fontsize=11, fontname="korea")
        a.update()
    # 숨긴 글상자 — 화면에는 안 보이지만 사이냅이 읽으므로 docparse 도 읽는다(0.2.0)
    hid = p1.add_freetext_annot(pymupdf.Rect(60, 300, 420, 330), "숨긴 글상자도 읽는다",
                                fontsize=11, fontname="korea")
    hid.set_flags(pymupdf.PDF_ANNOT_IS_HIDDEN)
    hid.update()
    # 쪽지(Text) 주석 — 접힌 검토 의견이라 읽지 않아야 한다
    note = p1.add_text_annot((60, 400), "쪽지 주석은 읽지 않는다")
    note.update()
    w = pymupdf.Widget()
    w.field_type = pymupdf.PDF_WIDGET_TYPE_TEXT
    w.field_name = "담당"
    w.rect = pymupdf.Rect(60, 100, 300, 125)
    w.field_value = "서식 칸에 적은 값"
    p2.add_widget(w)
    c = pymupdf.Widget()
    c.field_type = pymupdf.PDF_WIDGET_TYPE_CHECKBOX
    c.field_name = "확인"
    c.rect = pymupdf.Rect(60, 150, 80, 170)
    c.field_value = True
    p2.add_widget(c)
    st = p3.add_stamp_annot(pymupdf.Rect(60, 100, 260, 160), stamp=0)
    st.update()
    doc.save(path, garbage=4, deflate=True)


#------------------------------------------------------------------
# 메모 docx 만들기 — 본문 밖 글(extra_text) 시험용
#=> 본문 2문단 · 메모 1개 · 머리글 · 꼬리글. 메모 글은 본문 뒤 · 머리글 앞에 나와야 한다.
#
# -in: path = 만들 파일 경로
#
# -out: 없음
# -out: error = python-docx(1.2 이상)가 없으면 ImportError / AttributeError
#------------------------------------------------------------------
def make_comment_docx(path):
    import docx
    d = docx.Document()
    d.sections[0].header.paragraphs[0].text = "메모 시험 문서 머리글"
    d.sections[0].footer.paragraphs[0].text = "메모 시험 문서 꼬리글"
    p = d.add_paragraph("이 문단에 검토 메모가 달려 있다.")
    d.add_paragraph("둘째 문단은 메모가 없다.")
    d.add_comment(p.runs, text="검토 메모: 수치 출처를 적을 것", author="시험자")
    d.save(path)


# SmartArt 데이터 부품 — 점(pt) 3개에 글. drawing(겉모양 캐시)에도 같은 글을 넣어,
# 읽는 쪽이 data 만 읽고 drawing 은 건너뛰는지(두 번 안 나오는지) 볼 수 있게 한다.
_DGM_STEPS = ["기획 단계", "개발 단계", "운영 단계"]
_DGM_DATA = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" '
    'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><dgm:ptLst>'
    '<dgm:pt modelId="{0}" type="doc"><dgm:prSet/><dgm:spPr/>'
    '<dgm:t><a:bodyPr/><a:p><a:endParaRPr/></a:p></dgm:t></dgm:pt>'
    + "".join('<dgm:pt modelId="{%d}"><dgm:prSet/><dgm:spPr/><dgm:t><a:bodyPr/><a:p><a:r>'
              '<a:t>%s</a:t></a:r></a:p></dgm:t></dgm:pt>' % (i + 1, t) for i, t in enumerate(_DGM_STEPS))
    + '</dgm:ptLst><dgm:cxnLst/><dgm:bg/><dgm:whole/></dgm:dataModel>')
_DGM_DRAWING = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<dsp:drawing xmlns:dsp="http://schemas.microsoft.com/office/drawing/2008/diagram" '
    'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><dsp:spTree>'
    '<dsp:sp><dsp:txBody><a:p><a:r><a:t>기획 단계</a:t></a:r></a:p></dsp:txBody></dsp:sp>'
    '</dsp:spTree></dsp:drawing>')
_CT_DATA = "application/vnd.openxmlformats-officedocument.drawingml.diagramData+xml"
_CT_DRAWING = "application/vnd.ms-office.drawingml.diagramDrawing+xml"
_RT_DATA = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramData"
_RT_DRAWING = "http://schemas.microsoft.com/office/2007/relationships/diagramDrawing"


#------------------------------------------------------------------
# OOXML 파일에 SmartArt 부품 끼워 넣기
#=> python-docx · python-pptx 는 SmartArt 를 만들지 못한다. 그래서 저장한 파일의 zip 에
#   data · drawing 부품을 더하고, 기준 부품의 관계와 콘텐츠 형식에 등록한다.
#   (그림 자리 graphicFrame 은 넣지 않는다 — 읽는 쪽은 관계만 따라가므로 시험에 충분하다)
#
# -in: path        = 고칠 OOXML 파일
# -in: rels        = 기준 부품의 관계 파일 이름("ppt/slides/_rels/slide1.xml.rels")
# -in: data        = 넣을 data 부품 이름
# -in: drawing     = 넣을 drawing 부품 이름
# -in: rel_data    = 관계에 적을 data Target
# -in: rel_drawing = 관계에 적을 drawing Target
#
# -out: 없음 (파일을 고쳐 쓴다)
# -out: error = 원본을 못 읽으면 OSError
#------------------------------------------------------------------
def _inject_smartart(path, rels, data, drawing, rel_data, rel_drawing):
    src = io.BytesIO(open(path, "rb").read())
    zin = zipfile.ZipFile(src)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as zout:
        for it in zin.infolist():
            body = zin.read(it.filename)
            if it.filename == rels:
                # 기준 부품 관계에 data · drawing 두 관계를 더한다
                add = ('<Relationship Id="rIdDgmData" Type="%s" Target="%s"/>'
                       '<Relationship Id="rIdDgmDrawing" Type="%s" Target="%s"/>'
                       % (_RT_DATA, rel_data, _RT_DRAWING, rel_drawing))
                body = body.decode("utf-8").replace("</Relationships>", add + "</Relationships>").encode("utf-8")
            elif it.filename == "[Content_Types].xml":
                add = ('<Override PartName="/%s" ContentType="%s"/><Override PartName="/%s" ContentType="%s"/>'
                       % (data, _CT_DATA, drawing, _CT_DRAWING))
                body = body.decode("utf-8").replace("</Types>", add + "</Types>").encode("utf-8")
            zout.writestr(it, body)
        zout.writestr(data, _DGM_DATA)
        zout.writestr(drawing, _DGM_DRAWING)


#------------------------------------------------------------------
# SmartArt pptx · docx 만들기 — 본문 밖 글(extra_text) 시험용
#=> pptx: 슬라이드 1(제목 + SmartArt 3단계 + 발표자 노트) · 슬라이드 2(제목)
#   docx: 본문 1문단 + SmartArt 3단계
#   SmartArt 글은 그 슬라이드 글 뒤(pptx) · 본문 뒤(docx)에 한 번씩만 나와야 한다.
#
# -in: pptx_path = 만들 pptx 경로
# -in: docx_path = 만들 docx 경로
#
# -out: 없음
# -out: error = python-pptx · python-docx 가 없으면 ImportError
#------------------------------------------------------------------
def make_smartart(pptx_path, docx_path):
    import docx
    from pptx import Presentation
    prs = Presentation()
    s1 = prs.slides.add_slide(prs.slide_layouts[5])
    s1.shapes.title.text = "업무 흐름"
    s1.notes_slide.notes_text_frame.text = "발표자 노트: 단계별 담당을 설명한다"
    s2 = prs.slides.add_slide(prs.slide_layouts[5])
    s2.shapes.title.text = "맺음말"
    prs.save(pptx_path)
    _inject_smartart(pptx_path, "ppt/slides/_rels/slide1.xml.rels",
                     "ppt/diagrams/data1.xml", "ppt/diagrams/drawing1.xml",
                     "../diagrams/data1.xml", "../diagrams/drawing1.xml")
    d = docx.Document()
    d.add_paragraph("업무 흐름은 아래 그림과 같다.")
    d.save(docx_path)
    _inject_smartart(docx_path, "word/_rels/document.xml.rels",
                     "word/diagrams/data1.xml", "word/diagrams/drawing1.xml",
                     "diagrams/data1.xml", "diagrams/drawing1.xml")


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
    force = "--force" in sys.argv[1:]
    # (파일 이름, 만드는 함수) — SmartArt 는 pptx · docx 두 파일을 한 번에 만든다
    jobs = [
        ("글자층_예시.pdf", lambda: make_text_pdf(os.path.join(OUT, "글자층_예시.pdf"))),
        ("웹문서_예시.html", lambda: make_html(os.path.join(OUT, "웹문서_예시.html"))),
        ("묶음_예시.zip", lambda: make_zip(os.path.join(OUT, "묶음_예시.zip"))),
        ("주석_예시.pdf", lambda: make_annot_pdf(os.path.join(OUT, "주석_예시.pdf"))),
        ("메모_예시.docx", lambda: make_comment_docx(os.path.join(OUT, "메모_예시.docx"))),
        ("SmartArt_예시.pptx", lambda: make_smartart(os.path.join(OUT, "SmartArt_예시.pptx"),
                                                    os.path.join(OUT, "SmartArt_예시.docx"))),
    ]
    for name, make in jobs:
        # 이미 있으면 건너뛴다 — 다시 만들면 바이트가 바뀌어 git 에 잡힌다
        if force or not os.path.exists(os.path.join(OUT, name)):
            make()
            print("만듦:", name)
    print("폴더:", OUT)


if __name__ == "__main__":
    main()
