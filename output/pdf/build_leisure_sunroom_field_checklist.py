from pathlib import Path
from html import escape
import json
import hashlib
from reportlab.pdfgen import canvas
from reportlab.lib.pagesizes import A4
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.platypus import Paragraph
from reportlab.lib.styles import ParagraphStyle
from pypdf import PdfReader

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'output/pdf/HomeSense-休闲区及阳光房-实体按键与LED现场验收表.pdf'
QA = ROOT / 'tmp/pdfs/field-acceptance61-confirmed/field'
QA.mkdir(parents=True, exist_ok=True)
ACCEPTANCE = json.loads((ROOT / 'docs/leisure-sunroom-field-acceptance.json').read_text())
assert ACCEPTANCE['configuration_revision'] == 61
assert ACCEPTANCE['evidence_kind'] == 'user_reported_physical_field_acceptance'
assert ACCEPTANCE['result'] == 'all_passed'
assert ACCEPTANCE['total'] == ACCEPTANCE['passed'] == 24
assert ACCEPTANCE['failed'] == ACCEPTANCE['untested'] == 0
PASSED = [case for group in ACCEPTANCE['groups'] for case in group['cases'] if group['result'] == 'passed']
assert len(PASSED) == len(set(PASSED)) == 24
pdfmetrics.registerFont(TTFont('CN', '/System/Library/Fonts/Supplemental/Arial Unicode.ttf'))
W, H = A4
L, R = 42, W-42
WIDTH = R-L
INK = '#202A32'
GRAY = '#64717A'
RULE = '#BAC3C9'
C = canvas.Canvas(str(OUT), pagesize=A4)
C.setTitle('HomeSense 休闲区及阳光房 实体按键与LED现场验收表')
C.setAuthor('HomeSense')
COUNTS = []

def text(x, y, value, size=10.5, color=INK):
    C.setFillColor(color)
    C.setFont('CN', size)
    C.drawString(x, y, value)

def para(x, top, value, width=WIDTH, size=10.5, leading=15):
    p = Paragraph(escape(value).replace('\n', '<br/>'), ParagraphStyle('p', fontName='CN', fontSize=size, leading=leading, textColor=INK))
    _, h = p.wrap(width, 1000)
    p.drawOn(C, x, top-h)
    return top-h

def line(y, x=L, width=WIDTH, dashed=False):
    C.setStrokeColor(RULE)
    C.setLineWidth(.5)
    C.setDash(1, 2) if dashed else C.setDash()
    C.line(x, y, x+width, y)
    C.setDash()

def check(x, y, label, checked=False):
    C.setStrokeColor(INK)
    C.setLineWidth(.7)
    C.rect(x, y-1, 8, 8, stroke=1, fill=0)
    if checked:
        p = C.beginPath()
        p.moveTo(x+1.5, y+2.5)
        p.lineTo(x+3.4, y+.5)
        p.lineTo(x+6.8, y+5.4)
        C.drawPath(p)
    text(x+13, y-1, label, 9)

def page(number, title, subtitle):
    text(L, H-38, 'HomeSense / 现场验收', 9, GRAY)
    text(R-112, H-38, '基准：rev61', 9, GRAY)
    text(L, H-70, title, 19)
    para(L, H-84, subtitle, size=9.5, leading=13)
    line(H-112)
    line(42)
    text(L, 26, '用户现场确认：2026-10-29  |  24/24通过  |  响应秒数及签名未提供', 8, GRAY)
    text(R-42, 26, f'{number} / 6', 8, GRAY)

def card(top, code, title, action, expected, height=102):
    text(L+10, top-17, f'{code}  {title}', 11.2)
    y = para(L+10, top-24, '操作：'+action, WIDTH-20, 9.6, 13)
    y = para(L+10, y-3, '预期：'+expected, WIDTH-20, 9.6, 13)
    assert y >= top-height+33, (code, top-y)
    by = top-height+22
    assert code in PASSED
    for x, label in [(L+10, '通过'), (L+78, '异常'), (L+146, '未测')]:
        check(x, by, label, checked=label == '通过')
    text(L+220, by-1, '响应：灯光____秒 / LED____秒', 9)
    text(L+10, top-height+7, '备注／异常编号：', 9, GRAY)
    line(top-height+5, L+95, WIDTH-105, True)
    line(top-height)
    COUNTS.append(code)

page(1, '休闲区、阳光房 · 实体按键与LED验收', '结果：24项全部通过。依据用户现场人工测试后的明确确认；通过框已标记，空白处保留供补记。')
text(L, H-140, '确认日期：2026-10-29（北京时间）  操作人：____________  记录人：____________', 10)
text(L, H-166, '测试开始／结束时刻（未提供）：________________  配置基准：rev61', 10)
text(L, H-198, '面板定位与共享按键', 12)
rows = [
    ('0x5B  F1南门口', '4键 日常 → 休闲区；2键 阳光房 → 阳光房'),
    ('0x5A  F1卧室门口', '3键 日常；4键 娱乐；5键 会客'),
    ('0x57  F1楼梯口', '3键 日常；5键 全开；1键 会客'),
    ('0x56  F1影音室门口', '3键 日常；5键 全开；1键 会客'),
    ('0x5C  F1入户门', '1键 客厅 → 休闲区；3键 日常 → 阳光房'),
]
y=H-214
for a,b in rows:
    text(L+8,y-15,a,10)
    text(L+158,y-15,b,10)
    line(y-25)
    y-=34
y=para(L,y-14,'键号：面向面板，按 1 2 ／ 3 4 ／ 5 6 排列；左手为左，右手为右。',size=10)
y=para(L,y-10,'LED：选中应为红色，关闭后退出红色选中。既看按下时的及时响应，也看灯光稳定后的共享一致性；逐颗观察，不能仅看本地按键。',size=10)
y=para(L,y-10,'现场结论：用户确认“所有测试都符合预期”，并明确确认本表24项全部通过；异常0项、未测0项。逐项响应秒数、具体测试时刻及签名未提供，留空不代填。',size=10)
y=para(L,y-10,'配置依据：2026-10-28已发布rev61及当日09:11:50只读核对。该核对确认配置与清单一致，未代替实体灯光和LED的现场验收。',size=9.5)
text(L,y-30,'补充记录（未提供初始／最终灯态等逐项细节，保留空白）：',10)
for yy in [y-52,y-76,y-100]: line(yy,dashed=True)
C.showPage()

page(2, '01 / 休闲区“日常” · 五处共享', '每项开始时休闲区灯全灭。五处共享键：南门口4、卧室门口3、楼梯口3、影音室门口3、入户门1“客厅”。')
pairs=[('D1','南门口4 → 卧室门口3','南门口4键开启；观察后，用卧室门口3键关闭。'),
('D2','卧室门口3 → 楼梯口3','卧室门口3键开启；观察后，用楼梯口3键关闭。'),
('D3','楼梯口3 → 影音室门口3','楼梯口3键开启；观察后，用影音室门口3键关闭。'),
('D4','影音室门口3 → 入户门1','影音室门口3键开启；观察后，用入户门1键“客厅”关闭。'),
('D5','入户门1“客厅” → 南门口4','入户门1键开启；观察后，用南门口4键关闭。')]
for i,(code,title,action) in enumerate(pairs):
    card(H-123-i*109,code,title,action,'开启仅主灯100%，筒灯、灯带、过道灯关闭；五颗LED共同选中。关闭后休闲区灯全灭，五颗LED均退出选中。')
card(H-123-5*109,'D6','同键再次按下关闭','任选上述一颗键，按下开启，等待灯光稳定后，再按同一颗键。','第一次为日常配方，第二次关闭休闲区灯；五颗共享LED同步退出选中。备注写明所测面板和键号。')
C.showPage()

page(3,'02 / 休闲区“全开” · 两处联动','两处全开键：楼梯口5、影音室门口5。全开范围为休闲区主灯、筒灯-1/2、灯带-1/2和过道灯。')
full='开启时上述六组灯均100%，两颗5键LED共同选中；关闭时上述灯全灭，两颗LED退出选中。'
cases=[
('F1','楼梯口开，影音室门口关','从休闲区全灭开始，楼梯口5键开启；观察后，用影音室门口5键关闭。',full),
('F2','影音室门口开，楼梯口关','从休闲区全灭开始，影音室门口5键开启；观察后，用楼梯口5键关闭。',full),
('F3','楼梯口5键，同键重复','从休闲区全灭开始，楼梯口5键开启，稳定后再按同一键。',full),
('F4','影音室门口5键，同键重复','从休闲区全灭开始，影音室门口5键开启，稳定后再按同一键。',full),
('F5','日常切到全开','先用任一日常键开启仅主灯，再按任一全开键。','六组灯均100%；两颗全开LED选中，五颗日常／客厅LED退出选中。'),
('F6','全开切回日常','保持F5全开状态，按任一日常／客厅键。','仅主灯100%，其余休闲区灯关闭；五颗日常LED选中，两颗全开LED退出选中。随后再按日常键关闭。')]
for i,c in enumerate(cases): card(H-123-i*109,*c)
C.showPage()

page(4,'03 / 阳光房“日常” · 双键与双灯','两处场景键：南门口2“阳光房”、入户门3“日常”。单灯键：入户门2壁灯、入户门4吊灯。')
cases=[
('S1','南门口开，入户门关','从壁灯、吊灯均关闭开始，南门口2键开启，再用入户门3键关闭。','开启时壁灯、吊灯都亮，两颗场景LED选中；关闭时两灯全灭，两颗LED退出选中。'),
('S2','入户门开，南门口关','从两灯均关闭开始，入户门3键开启，再用南门口2键关闭。','灯光及双LED结果同S1；休闲区灯光不改变。'),
('S3','同键再次按下关闭','任选一颗阳光房场景键，开启并等待稳定，再按同一颗键。','第二次关闭壁灯和吊灯，两颗场景LED均退出选中。备注写明所测键。'),
('S4','单开壁灯，再补开吊灯','从两灯全灭开始，按入户门2键开壁灯；观察后，按入户门4键开吊灯。','仅壁灯亮时，两颗场景LED均不选中；补开吊灯、两灯都亮后，两颗LED共同选中。'),
('S5','从双灯反馈状态关闭','保持S4两灯全亮，按任一阳光房场景键。','壁灯、吊灯全部关闭，两颗场景LED退出选中。'),
('S6','单开吊灯，再补开壁灯','从两灯全灭开始，入户门4键开吊灯，再用2键补开壁灯，最后用场景键关闭。','仅吊灯亮时双LED不选中；两灯都亮时双LED选中；场景键关闭两灯并复位双LED。')]
for i,c in enumerate(cases): card(H-123-i*109,*c)
C.showPage()

page(5,'04 / 两房独立性与相邻场景回归','日常指休闲区五处共享键；阳光房指南门口2或入户门3。按预期检查两组灯和两组LED。')
cases=[
('I1','两个日常同时开启','从两房灯全灭开始，先开休闲区日常，再开阳光房日常。','主灯100%，壁灯和吊灯亮；休闲区五颗LED、阳光房两颗LED各自选中。'),
('I2','关闭休闲区，保留阳光房','保持I1状态，按任一休闲区日常键关闭。','休闲区灯全灭、五颗LED退出选中；阳光房两灯及其两颗LED保持。'),
('I3','关闭阳光房，保留休闲区','保持I2，重开休闲区日常，再按阳光房场景键关闭阳光房。','休闲区仅主灯亮、五颗LED保持选中；阳光房两灯关闭、两颗LED退出选中。'),
('I4','休闲区全开／关闭不干扰阳光房','先开阳光房日常，再用休闲区全开键开启、关闭休闲区。','全开及关闭仅改变休闲区；阳光房壁灯、吊灯和两颗场景LED始终保持开启／选中。'),
('R1','卧室门口4键“娱乐”','人工确认休闲区全灭，按卧室门口4键，观察后再按一次。','开启主灯、筒灯-1/2、过道灯100%，两路灯带关闭；娱乐键选中。再次按下关闭并退出选中。'),
('R2','三处“会客”共享','卧室门口5键开启、楼梯口1键关闭；再由影音室门口1键开启、卧室门口5键关闭。','仅主灯、筒灯-1/2为100%，灯带和过道灯关闭；三处会客LED共同选中，关闭后共同退出。')]
for i,c in enumerate(cases): card(H-123-i*109,*c)
C.showPage()

page(6,'05 / 异常明细与验收签认','本次24项全部通过，未报告异常。以下保留为后续补记栏；具体测试时刻和签名未提供，不代填。')
top=H-126
for i in range(4):
    y=top-i*132
    text(L,y-14,f'异常 A{i+1}    对应步骤：________    时间：____________    面板／键号：________________',10)
    text(L,y-39,'操作前灯态及按键操作：',9.5)
    line(y-43,L+112,WIDTH-112,True)
    text(L,y-64,'实际亮灯／未联动LED／延迟：',9.5)
    line(y-68,L+143,WIDTH-143,True)
    text(L,y-89,'复测结果／处理记录：',9.5)
    line(y-93,L+103,WIDTH-103,True)
    line(y-117)
text(L,165,'验收汇总（共24项）',12)
text(L,142,'通过：24项    异常：0项    未测：0项    待复测：无（据用户本次确认）',10)
check(L,113,'全部通过', checked=True)
check(L+120,113,'部分通过，保留异常')
check(L+318,113,'待复测')
text(L,80,'现场最终保留灯态（待补）：________________    本人签名（待填）：____________',10)
C.showPage()
C.save()
assert len(COUNTS)==24 and len(set(COUNTS))==24, COUNTS
assert COUNTS == PASSED
r=PdfReader(OUT)
assert len(r.pages)==6
combined='\n'.join(p.extract_text() for p in r.pages)
assert all(x in combined for x in COUNTS)
report={'pdf':str(OUT),'pages':6,'cases':COUNTS,'field_acceptance':ACCEPTANCE,'size':OUT.stat().st_size,'sha256':hashlib.sha256(OUT.read_bytes()).hexdigest()}
(QA/'verification.json').write_text(json.dumps(report,ensure_ascii=False,indent=2))
print(json.dumps(report,ensure_ascii=False,indent=2))
