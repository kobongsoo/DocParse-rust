# -*- coding: utf-8 -*-
"""docparse Python 예제 — 압축은 펼치고, 나온 문서마다 본문 추출.

    python example.py a.hwp b.zip

묶음의 python/docparse.py 를 프로젝트에 복사하고, lib/ 의 라이브러리 위치를 알려 준다
(docparse.py 옆, 그 아래 lib/, 또는 환경변수 DOCPARSE_LIB).
"""
import os
import shutil
import sys
import tempfile

import docparse


#------------------------------------------------------------------
# 문서 하나 추출해 요약 한 줄 찍기
#=> 크기 상한 초과와 그 밖의 실패를 나눠 보여 준다.
#
# -in: src   = 실제 파일 경로
# -in: label = 화면에 보일 이름(압축 안 문서면 "a.zip/안.hwp")
#
# -out: 없음
# -out: error = 없음(실패는 한 줄로 찍고 넘어간다)
#------------------------------------------------------------------
def show(src, label):
    try:
        # clean=True: 6단계 정제(이 감싸개의 기본은 False 다)
        r = docparse.extract(src, clean=True, max_pdf_pages=500, label=label)
    except docparse.SizeLimitError as e:
        print("%s : 크기 초과 %d > %d" % (label, e.size, e.limit))
        return
    except docparse.ExtractError as e:
        print("%s : %s — %s" % (label, getattr(e, "kind", "failed"), e))
        return
    print("%s : %s %d자 | %s" % (label, r.fmt, len(r.text), r.text[:40].replace("\n", " ")))


#------------------------------------------------------------------
# 예제 본체
#=> 1) 라이브러리를 불러 버전을 찍고 로그를 연결한다
#    2) 모든 입력을 expand_paths 로 펼친다(압축이 아니면 그대로 돌아온다)
#    3) 나온 문서마다 show
#
# -in: argv = 명령줄 인자(파일 경로들)
#
# -out: 없음
# -out: error = 라이브러리를 못 찾으면 OSError
#------------------------------------------------------------------
def main(argv):
    docparse.load()  # 생략해도 첫 호출 때 알아서 찾는다
    print("docparse %s (%s)" % (docparse.version(), docparse.lib_path()))
    # 경고 이상 로그만 표준오류로
    docparse.set_logger(lambda lv, target, site, msg: sys.stderr.write("[docparse %d] %s\n" % (lv, msg)), 2)

    tmp = tempfile.mkdtemp(prefix="docparse-")
    try:
        jobs, stats = docparse.expand_paths(argv, tmp, max_depth=3)
        for src, label, _origin in jobs:
            show(src, label)
        print("펼치지 못함:", stats["unexpanded"])
    finally:
        # 펼친 파일은 다 읽은 뒤 지운다
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main(sys.argv[1:])
