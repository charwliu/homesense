from pathlib import Path
import collections
import hashlib
import json
import math
import shutil
from xml.sax.saxutils import escape

from reportlab.lib import colors
from reportlab.lib.pagesizes import A4
from reportlab.lib.styles import ParagraphStyle
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.platypus import (
    Flowable, KeepTogether, PageBreak, Paragraph, SimpleDocTemplate,
    Spacer, Table, TableStyle,
)
from pypdf import PdfReader

ROOT = Path('/Users/liuwei/works/homesense')
PRIVATE = ROOT / 'deployment/ngr/private/20261017-all28-takeover-root'
FIELD = PRIVATE / 'next-release-field-changes-20261020/composed-candidate41'
LIVE = Path('/Users/liuwei/works/homesense/deployment/ngr/private/20261017-all28-takeover-root/incident-theater-20261023')
OUT = ROOT / 'output/pdf/HomeSense-Keypad-485-配电间及灯光窗帘通道清单.pdf'
QA = ROOT / 'tmp/pdfs/field-acceptance61-confirmed/channel'
QA.mkdir(parents=True, exist_ok=True)
BASE = QA.parent / OUT.name
field_acceptance = json.loads((ROOT/'docs/leisure-sunroom-field-acceptance.json').read_text())
assert field_acceptance['configuration_revision'] == 61
assert field_acceptance['evidence_kind'] == 'user_reported_physical_field_acceptance'
assert field_acceptance['result'] == 'all_passed'
assert field_acceptance['total'] == field_acceptance['passed'] == 24
assert field_acceptance['failed'] == field_acceptance['untested'] == 0
assert sum(len(group['cases']) for group in field_acceptance['groups']) == 24

config = json.loads((LIVE / 'dining51.devices.json').read_text())
automation = json.loads((LIVE / 'dining51.automation.json').read_text())
accepted = json.loads((FIELD / 'FIELD-ALL41-COMPOSED-ACCEPTANCE-RESULT.json').read_text())
assert config['revision'] == automation['configuration_revision'] == 51
assert hashlib.sha256((LIVE/'dining51.automation.json').read_bytes()).hexdigest() == '4056f700ca07666de4310dda758d105da21497b261b8011e9ca599437a37b819'
assert hashlib.sha256((LIVE / 'dining51.devices.json').read_bytes()).hexdigest() == '2f2d7f588605830196a01352a89aea549a2f7aca0b161fd12d12793fcff66670'
assert next(r for r in config['rooms'] if r['id'] == 'control4-room-401') == {'id':'control4-room-401','floor':'3层','name':'起居室'}
assert accepted['new_card_count'] == 41 and accepted['group_count'] == 14
assert accepted['state'] == 'basic_field_acceptance_passed'
CURRENT = ROOT / 'deployment/ngr/private/20261028-sunroom-key-alias61'
current_config = json.loads((CURRENT/'candidate.devices.json').read_text())
current_automation = json.loads((CURRENT/'candidate.automation.json').read_text())
current_result = json.loads((CURRENT/'FINAL-RESULT.json').read_text())
assert current_config['revision'] == current_automation['configuration_revision'] == current_result['revision'] == 61
assert current_result['completed'] and current_result['real_actuator_tests'] == 0
AUDIT = ROOT / 'deployment/ngr/private/20261028-live-scene-document-audit'
readonly_audit = json.loads((AUDIT/'AUDIT-RESULT.json').read_text())
readonly_snapshot = json.loads((AUDIT/'LIVE-READONLY.json').read_text())
assert readonly_audit['passed'] and readonly_audit['revision'] == 61
assert readonly_audit['affected_panels'] == 5
assert readonly_audit['printed_labels_compared'] == 30 and readonly_audit['bindings_compared'] == 16
assert readonly_audit['shared_selection_and_led_bindings_match']
assert readonly_audit['control_requests'] == readonly_audit['module_queries_sent'] == readonly_audit['hardware_writes_requested'] == 0
assert all(request['method'] == 'GET' and request['status'] == 200 for request in readonly_snapshot['requests'])
assert readonly_audit['source_files_match_active_sqlite_configuration']
assert readonly_audit['live_input_hashes_match_document_generation']
for name in ['devices', 'automation']:
    assert hashlib.sha256((CURRENT/f'candidate.{name}.json').read_bytes()).hexdigest() == readonly_snapshot['file_hashes'][name]
audit_time = readonly_audit['checked_at_local'][:19].replace('T', ' ')
current_modules = {m['id']:m for g in current_config['gateways']for m in g['modules']}
assert current_config['gateways'] == config['gateways']
assert current_config['rooms'] == config['rooms']
# Module addresses, cabinet positions and curtain device definitions are unchanged.
old_motors = [d for d in config['devices']if next(m['profile']for g in config['gateways']for m in g['modules']if m['id']==d['module_id']).startswith('c4_')]
new_motors = [d for d in current_config['devices']if current_modules[d['module_id']]['profile'].startswith('c4_')]
assert old_motors == new_motors
for panel in current_config['keypads']:
 previous = next(p for p in config['keypads']if p['id']==panel['id'])
 assert (panel['module_id'],panel['room'],panel['name']) == (previous['module_id'],previous['room'],previous['name'])
modules = {m['id']: m for g in config['gateways'] for m in g['modules']}
devices = {d['id']: d for d in config['devices']}
rooms = {r['id']: r for r in config['rooms']}

def address(device):
    d = devices[device] if isinstance(device, str) else device
    return f"{int(modules[d['module_id']]['address'], 16):02X}-{d['channel']}"

def room_title(room):
    r = rooms[room]
    return f"{r['floor']} {r['name']}"

def room_sort(room):
    r = rooms[room]
    return (int(r['floor'][0]), r['name'])

lighting = {address(d): d for d in config['devices']
            if int(modules[d['module_id']]['address'], 16) in [*range(1, 8), *range(17, 22)]}
assert len(lighting) == 96
accepted_groups = {g['id'] for g in accepted['groups']}
rows, covered = [], set()
for group in config['composed_lights']:
    assert group['id'] in accepted_groups
    for light in group['lights']:
        d = devices[light['id']]
        for index, member in enumerate(light['members']):
            supply, dimmer = address(member['supply']), address(member['dimmer'])
            covered.update([supply, dimmer])
            name = light['name']
            note = '现网逻辑灯；基础开关/读回验收通过'
            basis = '基础验收'
            if group['id'] == 'composed-master':
                basis = '逐灯实测'
                if name == '夜灯':
                    name += '-左' if supply == '06-1' else '-右'
                    note = '两行合为一张“夜灯”卡片；共用调光'
                elif name.startswith('阅读'):
                    note = '左右供电独立，阅读共用15-8调光'
                else:
                    note = '主灯/阳台共用01-8供电，各自调光'
            elif group['id'] == 'composed-north':
                if name == '夜灯':
                    name += '-左' if supply == '06-7' else '-右'
                    note = '两行合为一张“夜灯”卡片，共用11-7'
                else:
                    note = '左右供电独立，阅读共用15-6调光'
            elif group['id'] == 'composed-south':
                note = '阅读/夜灯共用05-7供电' if name.startswith('阅读') else '夜灯/筒灯共用12-8调光；场景互斥保留'
            rows.append(dict(room=d['room'], name=name, supply=supply, dimmer=dimmer, basis=basis, note=note))

for d in config['devices']:
    if d.get('supply'):
        supply = address(d['supply'])
        dimmer = address(d)
        covered.update([supply, dimmer])
        rows.append(dict(room=d['room'], name=d['name'], supply=supply, dimmer=dimmer,
                         basis='既有记录', note='现网供电/调光绑定；非本轮新增卡片'))

# Physical fixtures that share dimmers with the five master logical cards.
master = next(r['id'] for r in config['rooms'] if r['name'] == '主卧')
for supply, name in [('06-1', '左中间灯'), ('06-2', '右中间灯')]:
    rows.append(dict(room=master, name=name, supply=supply, dimmer='15-5', basis='逐灯实测',
                     note='实际灯具；无独立卡片，可能随阳台调光联亮'))
    covered.update([supply, '15-5'])

retained = {'control4:348', 'control4:385', 'control4:71', 'control4:405'}
for port, d in lighting.items():
    if port in covered:
        continue
    supply = port if int(port[:2], 16) < 0x10 else ''
    note, basis = '现网保留；未据配置认定逐灯验收', '现网配置'
    if d['id'] in retained:
        note, basis = '按用户决定保留原用途/配对；不猜测、不作为结项待核项', '保留'
    elif port == '01-2':
        note, basis = '排气/风机，非灯光；已批准随离家关闭', '用途确认'
    elif port == '04-8':
        note, basis = '“花园”；画室5键及设备间1键联动', '按键实测'
    if not d.get('catalog_visible', True):
        note += '；原始卡片隐藏'
    rows.append(dict(room=d['room'], name=d['name'], supply=supply,
                     dimmer='' if supply else port, basis=basis, note=note))
    covered.add(port)
assert covered == set(lighting), (set(lighting) - covered, covered - set(lighting))
assert len([r for r in rows if r['basis'] == '保留']) == 4
assert not any('Front' in room_title(r['room']) for r in rows)
rows.sort(key=lambda r: (*room_sort(r['room']), r['supply'], r['dimmer'], r['name']))

pdfmetrics.registerFont(TTFont('CN', '/System/Library/Fonts/Supplemental/Arial Unicode.ttf'))
NAVY = colors.HexColor('#15334C')
TEAL = colors.HexColor('#087F8C')
GRAY = colors.HexColor('#586977')
LINE = colors.HexColor('#D2DEE5')
PALE = colors.HexColor('#F0F5F7')
AMBER = colors.HexColor('#8A5417')
W, H = A4
WIDTH = W - 72
styles = {
    'title': ParagraphStyle('title', fontName='CN', fontSize=20, leading=28, textColor=NAVY, spaceAfter=9),
    'sub': ParagraphStyle('sub', fontName='CN', fontSize=10, leading=16, textColor=GRAY, spaceAfter=10),
    'head': ParagraphStyle('head', fontName='CN', fontSize=13, leading=19, textColor=TEAL, spaceBefore=12, spaceAfter=7),
    'body': ParagraphStyle('body', fontName='CN', fontSize=9.5, leading=15, textColor=NAVY, spaceAfter=7),
    'cell': ParagraphStyle('cell', fontName='CN', fontSize=8.6, leading=12.5, textColor=NAVY),
    'small': ParagraphStyle('small', fontName='CN', fontSize=8, leading=12, textColor=GRAY, spaceAfter=5),
    'white': ParagraphStyle('white', fontName='CN', fontSize=8.7, leading=13, textColor=colors.white),
}

def para(text, style='body'):
    return Paragraph(escape(str(text)).replace('\n', '<br/>'), styles[style])

def title(text, sub=None):
    result = [para(text, 'title')]
    if sub:
        result.append(para(sub, 'sub'))
    return result

def table(headers, data, widths, font='cell', pad=5):
    result = Table([[para(x, 'white') for x in headers]] +
                   [[para(x, font) for x in row] for row in data],
                   colWidths=widths, repeatRows=1, hAlign='LEFT')
    result.setStyle(TableStyle([
        ('BACKGROUND', (0, 0), (-1, 0), NAVY),
        ('ROWBACKGROUNDS', (0, 1), (-1, -1), [colors.white, PALE]),
        ('LINEBELOW', (0, 0), (-1, -1), .35, LINE),
        ('VALIGN', (0, 0), (-1, -1), 'TOP'),
        ('LEFTPADDING', (0, 0), (-1, -1), pad),
        ('RIGHTPADDING', (0, 0), (-1, -1), pad),
        ('TOPPADDING', (0, 0), (-1, -1), pad),
        ('BOTTOMPADDING', (0, 0), (-1, -1), pad),
    ]))
    return result

class Cabinets(Flowable):
    def __init__(self):
        super().__init__()
        self.width, self.height = WIDTH, 330
    def draw(self):
        c = self.canv
        gap = 14
        panel_w = (WIDTH - gap) / 2
        layouts = [
            ('左柜 · IMG_6887', [('04','R818'),('12','AD8'),('05','R818'),('02','R818'),('14','AD8'),('03','R818'),('01','R818'),None,None]),
            ('右柜 · IMG_6886', [('22','C4'),('15','AD8'),('23','C4'),('21','C4'),('13','AD8'),('06','R818'),('07','R818'),('11','AD8'),None]),
        ]
        for side, (name, blocks) in enumerate(layouts):
            x = side * (panel_w + gap)
            c.setStrokeColor(LINE); c.setFillColor(colors.white)
            c.roundRect(x, 19, panel_w, 302, 7, fill=1, stroke=1)
            c.setFillColor(NAVY); c.roundRect(x, 286, panel_w, 35, 7, fill=1, stroke=0)
            c.setFont('CN', 11); c.setFillColor(colors.white); c.drawString(x+10, 299, name)
            box_w = (panel_w - 34) / 3
            for i, item in enumerate(blocks):
                col, row = i % 3, i // 3
                bx, by = x+10+col*(box_w+7), 207-row*86
                if item is None:
                    c.setFillColor(PALE); c.roundRect(bx, by, box_w, 72, 5, fill=1, stroke=0)
                    if col == 1 or side == 1:
                        c.setFillColor(GRAY); c.setFont('CN', 7); c.drawString(bx+6, by+31, '电源/网关区域')
                    continue
                addr, kind = item
                fill, ink = {'R818':('#E8F2F9','#246C9A'),'AD8':('#FAF1E5','#A56819'),'C4':('#E5F3EF','#078575')}[kind]
                c.setFillColor(colors.HexColor(fill)); c.setStrokeColor(LINE)
                c.roundRect(bx, by, box_w, 72, 5, fill=1, stroke=1)
                c.setFillColor(colors.HexColor(ink)); c.setFont('CN', 15); c.drawString(bx+8, by+45, '0x'+addr)
                c.setFillColor(NAVY); c.setFont('CN', 9); c.drawString(bx+8, by+20, kind)
        c.setFont('CN', 8); c.setFillColor(GRAY)
        c.drawString(0, 3, '正面相对位置沿用用户确认的柜位；不代表RS485串接顺序。')

story = title('Keypad 485 / 配电间\n灯光与窗帘通道清单', '场景及面板rev61 · 用户现场确认24项通过：2026-10-29')
story += [para('本版同步休闲区、阳光房场景及面板按键；六键名称按已发布rev61列示。柜位图、物理供电/调光配对和窗帘通道沿用既有核对记录，验收范围保留原记录；本次文档更新未操作设备。'),
 table(['核查项目','本版记录'],[
 ['配置与面板','场景/按键依据rev61；28块TS6-86面板、168键，按485地址升序排列。入户门1键“客厅”沿用休闲区日常；南门口2键“阳光房”共用入户门3键的阳光房日常。'],
 ['照明与窗帘','12个照明模块、96路输出；11路窗帘。灯光供电/调光表保留既有71条物理配对或单通道记录，未因场景改名调整硬件地址。'],
 ['场景与HomeKit','35项公共场景、111项HAP配件，HomeKit配置号21。休闲区日常HS-89，阳光房日常HS-160；休闲区娱乐HS-94，全部HS-159。既有配对和凭证继续使用。'],
 ['运行与验收','NanoPi运行Release 1f4f32d5，场景采用batch-query。rev60本机38项、rev61本机24项模拟回归通过；均完成两轮96路反馈及HAP/按键只读验收，未自动控制灯光或窗帘。'],
 ['用户现场验收','2026-10-29用户确认休闲区、阳光房24项实体按键及LED验收全部通过：24通过、0异常、0未测。逐项秒数、具体测试时刻及签名未提供；范围不扩大到其他房间。'],
 ['柜位与方向','保留原模块位置图：面向柜体正面，IMG_6887在左、IMG_6886在右；面向面板，左手为左、右手为右。'],
 ['历史通道记录','逐灯实测、基础验收、既有绑定和用户保留项继续分别标注；其他房间的历史配对关系保留，不扩大为本轮现场通过。'],
 ],[88,WIDTH-88]),
 para('本次更新重点','head'),
 para('休闲区日常仅开主灯100%，其余休闲区灯关闭；阳光房日常开壁灯和吊灯，两房独立。影音室门口5键“全开”与楼梯口5键共用配方及LED，任一键再次按下关闭同一范围。'),
 para('阅读提示','head'),
 para('下页列场景配方、通道及联动键；其后是485地址、配电间位置图、原有分层灯光/输出与窗帘通道，末尾列28块面板六键名称。名称表用于查找，具体动作以场景及联动说明为准。','small'),PageBreak()]

story += title('休闲区 / 阳光房 · 场景与联动', '依据已发布rev60/61；名称、实际配方和联动键分别列示')
story += [table(['房间 / 场景','开启配方','实体面板键 / HomeKit'],[
 ['休闲区 / 日常','主灯100%；筒灯-1、筒灯-2、灯带-1、灯带-2和过道灯关闭。','南门口4；卧室门口3；楼梯口3；影音室门口3；入户门1（客厅）。共用状态和LED。HS-89。'],
 ['休闲区 / 全开','主灯、筒灯-1、筒灯-2、灯带-1、灯带-2、过道灯全部100%。','楼梯口5、影音室门口5（由会客改名）。共用配方和LED；再按任一键关闭同一范围。App/Home显示“全部”，HS-159。'],
 ['休闲区 / 娱乐','主灯、筒灯-1、筒灯-2、过道灯100%；两路灯带关闭。','卧室门口4（娱乐）；HS-94。仅主灯亮不判定为娱乐开启。'],
 ['休闲区 / 会客','主灯、筒灯-1、筒灯-2为100%；两路灯带和过道灯关闭。','卧室门口5、楼梯口1、影音室门口1。影音室门口5已改为全开。'],
 ['阳光房 / 日常','壁灯、吊灯开启；不改变休闲区灯光。','南门口2（阳光房）、入户门3（日常）。共用状态、重复关闭和双LED。HS-160。'],
 ],[82,204,WIDTH-286],pad=5),
 para('灯具与通道','head'),
 para('休闲区：主灯02-6 / 12-5；筒灯-1为02-3 / 14-7、筒灯-2为02-5 / 14-7；灯带-1为02-7 / 14-5、灯带-2为02-7 / 14-6；过道灯为05-1 / 15-1。阳光房：壁灯R818 04-4，吊灯R818 01-5。双地址统一按“R818供电 / AD8调光”列示；仅列R818的灯为开关控制。','small'),
 para('按键与LED规则','head'),
 para('入户门1键仅改名“客厅”，仍与其他休闲区日常/主灯键联动；南门口2键由壁灯单独控制改为阳光房日常。入户门2键仍单独控制壁灯，入户门4键及南门口6键仍单独控制吊灯。阳光房两颗场景键在两灯都亮时共同选中；只亮其中一灯时不显示场景选中。'),
 para('两房日常可以同时开启，各自关闭不会关闭另一房。回家配方仍开休闲区日常、阳光房壁灯和吊灯；本次仅调整场景键归属及指示联动，不增加其他负载。','small'),
 para('本轮证据','head'),
 para('rev60/61均已完成本机模拟与部署只读验收；场景读取最大约44ms / 46ms，500ms上限保持。实体灯光和LED另按下述用户现场确认记录；本次编制文档未发送控制命令。','small'),
 para('现网只读复核 · ' + audit_time + '（北京时间）','head'),
 para('核对时现网为rev61，配置文件、SQLite活动配置与运行接口一致，配置指纹与清单生成输入相同。'),
 para('已核对0x56、0x57、0x5A、0x5B、0x5C共5块面板的30个键名、16项动作绑定。日常、全开、娱乐、会客、阳光房日常及回家/关闭配方均与清单一致。'),
 para('两房日常的独立范围、全开与阳光房的共享选择/LED绑定均一致；35项公共场景、111项HAP配件一致。六项GET均返回200，单次场景目录读取约8ms。只读过程未发送模块查询、LED或灯光/窗帘控制，未重启或修改配置；此结论确认配置与绑定，不代替实体动作验收。','small'),
 para('用户现场验收 · 2026-10-29 · 24/24通过','head'),
 para('D1-D6休闲区日常共享；F1-F6全开双键及切换；S1-S6阳光房双键双灯与单灯反馈；I1-I4两房独立性；R1-R2娱乐/会客回归。用户人工逐项测试并确认全部符合预期：24通过、0异常、0未测。','small'),
 para('确认涵盖上述灯光配方、跨面板/重复关闭和实体LED联动。逐项响应秒数、具体测试时刻及签名未提供，未代填；其他房间及历史通道验收范围不变。详见固定文件名的独立实体按键与LED现场验收表。','small'),PageBreak()]

panels = sorted(current_config['keypads'], key=lambda p: int(modules[p['module_id']]['address'], 16))
assert len(panels) == 28 and sum(len(p['keys']) for p in panels) == 168
story += title('28块面板地址', 'TS6-86 · 六键排列：1 2 / 3 4 / 5 6；面向面板，左手为左，右手为右')
story += [table(['地址', '十进制', '楼层 / 房间', '现网面板名称', '原对象ID'], [
    ['0x'+modules[p['module_id']]['address'], int(modules[p['module_id']]['address'],16), room_title(p['room']), p['name'], p['id'].split(':')[1]]
    for p in panels], [49,46,121,240,WIDTH-456], pad=4),
    Spacer(1,9), para('左右名称以面向面板时左手为左、右手为右，已按现场修正：主卧0x4A为右、0x4B为左；南卧0x4D为左侧窗帘面板、0x4E为右侧场景面板；北卧0x4F为右侧窗帘面板、0x50为左侧场景面板。地址和动作不因改名互换。', 'small'), PageBreak()]

story += title('1层配电间 · 模块位置图', '面向柜体正面：左手侧为IMG_6887，右手侧为IMG_6886')
story += [Cabinets(), Spacer(1,14), table(['模块类别','地址','数量 / 说明'], [
    ['R818继电器/开关','0x01 - 0x07','7个；56路输出'],
    ['AD8调光','0x11 - 0x15','5个；40路输出'],
    ['C4-DIN窗帘','0x21、0x22、0x23','3个；现网使用11路窗帘'],
], [120,160,WIDTH-280]), para('总线与控制', 'head'),
    para('HomeSense连接192.168.0.10:8443；串口9600、8数据位、1停止位、无校验。R818状态按A1-A8逐路查询，AD8使用A0整模块查询；12模块/96路每轮61个查询帧。'),
    para('场景采用batch-query：先成批发送控制，再按模块查询更新状态。R818主动反馈已在此前发布中关闭，状态通过查询确认；AD8未套用R818参数。初始化及后台查询沿用已发布调度。本次未查询或修改模块参数。', 'small'), PageBreak()]

for floor in ['1层','2层','3层']:
    subset=[r for r in rows if rooms[r['room']]['floor']==floor]
    story += title(f'{floor} · 灯光 / 输出配对', '地址格式为十六进制模块号-十进制通道号，例如12-4表示AD8 0x12第4路。')
    story += [table(['房间','逻辑灯 / 实际灯具','R818\n供电','AD8\n调光','依据','共用关系 / 核查说明'],[
        [rooms[r['room']]['name'],r['name'],r['supply']or'-',r['dimmer']or'-',r['basis'],r['note']]for r in subset
    ], [52,98,49,49,63,WIDTH-311],pad=4), Spacer(1,7),
        para('同一地址出现在多行表示共用通道，不表示新增硬件。单一供电或调光行不自动与邻行配对。“隐藏”指原始设备卡片，不代表线路无负载。', 'small'), PageBreak()]

story += title('共享通道与场景规则', '区分“物理配对”“独立逻辑灯”和“场景配方”')
story += [table(['房间','当前确认的关系 / 控制语义'],[
    ['主卧','01-8给主灯/阳台供电；06-1左床头、06-2右床头。15-4对应主灯及两侧夜灯，15-5对应阳台及两侧中间灯，15-8对应两侧阅读。各行亮起取决于相应供电和调光同时有效。'],
    ['主卧','主灯卡片与阳台可各自关闭并保留另一盏；阅读中开/关阳台保留阅读，中间灯允许随动；夜灯中开阳台退出夜灯。墙上左3“主灯”与左2共用“日常”；全开含阳台/中间灯，转日常只留主灯。这些rev48配方变动仍待差异现场签认。'],
    ['南卧','阅读-左=05-7+15-7；阅读-右=05-7+12-7；夜灯=05-7+12-8；筒灯=05-8+12-8。场景入口的互斥规则保留，独立卡片的共用通道保护与场景互斥不能混为一谈。'],
    ['北卧','阅读-左=06-7+15-6；阅读-右=07-7+15-6。两侧夜灯分别为06-7/07-7供电配11-7调光，作为一张“夜灯”卡片显示。'],
    ['通用组合灯','关闭时保护仍被其他已开启逻辑灯使用的共享通道。不能有两张灯卡片同时共享同一供电和同一调光；通道共用仍可能有已知联亮，不宣称每盏实灯都完全独立。'],
    ['亮度策略','场景直接到目标亮度，不再使用渐变指令；夜灯/节能的非零、非100%目标统一10%，100%目标保持。夜灯卡片可手动到100%，不能把“夜灯场景10%”误写成硬件固定上限。'],
    ['全部 / 离家','“全部”按任意灯亮决定关闭，否则按已确认配方开启。全屋打开/关闭覆盖12模块96路含风机；开启逐模块等8路目标反馈及接收空闲再继续，缺反馈只查/重试当前模块，失败停后续；离家使用全屋关闭，窗帘不在范围。'],
], [66,WIDTH-66]),
    para('验收范围', 'head'), para('本页与分层物理通道表保留既有核对记录及其原验收范围。rev60/61的场景及按键软件/只读验收见前页，不把历史基础开关通过升级为本轮全部共享组合或实体LED通过。', 'small'), PageBreak()]

motors = [d for d in config['devices'] if modules[d['module_id']]['profile'].startswith('c4_')]
motors.sort(key=lambda d:(*room_sort(d['room']),address(d)))
assert len(motors)==11
story += title('11路窗帘通道', 'C4-DIN-PRO · 配置通道与按键映射')
motor_rows=[]
for d in motors:
    keys=[]
    for p in panels:
        for k in p['keys']:
            action=k['action']
            if action.get('kind')=='motor' and action.get('device')==d['id']:
                keys.append(f"0x{modules[p['module_id']]['address']} {k['number']}键 {k['label']}")
    motor_rows.append([room_title(d['room']),d['name'],'0x'+address(d),'；'.join(keys)])
story += [table(['楼层 / 房间','窗帘','模块-通道','现网面板键'],motor_rows,[82,85,76,WIDTH-243]),
    para('已确认的通用规则', 'head'),
    para('配置行程：开/关均10秒；0x21、0x22、0x23吸合时间按既有设置记录为18秒。电机到位自行停止，位置显示为估计；重启后未知。运动中同方向再次点按停止，独立STOP始终可操作。'),
    para('LED：点开/关时本方向立即红，对向先保持原色，约2秒后红；中停后开/关均红，停止键立即绿；估计全开为开绿/关红，估计全关为关绿/开红。画室只有开/关两键，同向再按停止。'),
    para('书房、画室及部分3层房间保留逐项历史实测。当前11路各5项命令已在本机回环检查方向/STOP/状态；不证明实际运动。本文未发设备指令，未逐帘观察的方向/位置不升级为实测结论。', 'small'), PageBreak()]

for page in range(2):
    group=panels[page*14:(page+1)*14]
    story += title(f'面板六键名称 · {page+1}/2', '键号按1 2 / 3 4 / 5 6排列；名称按rev61列示；休闲区/阳光房配方与联动见前页。')
    story += [table(['地址 / 面板','1','2','3','4','5','6'],[
        [f"0x{modules[p['module_id']]['address']}\n{p['name']}"]+
        [next(k['label']for k in p['keys']if k['number']==i)for i in range(1,7)]for p in group
    ],[109]+[(WIDTH-109)/6]*6,pad=5)]
    if page==0:story.append(PageBreak())

class NumberedCanvas:
    pass

def page_chrome(c, doc):
    c.saveState();c.setStrokeColor(LINE);c.setLineWidth(.5);c.line(36,35,W-36,35)
    c.setFont('CN',7.5);c.setFillColor(GRAY)
    c.drawString(36,23,'HomeSense · 场景及按键rev61 / 通道沿用既有记录 · 2026-10-28')
    c.drawRightString(W-36,23,str(doc.page))
    c.restoreState()

temporary=QA/'updated-before-validation.pdf'
doc=SimpleDocTemplate(str(temporary),pagesize=A4,rightMargin=36,leftMargin=36,topMargin=35,bottomMargin=47,
                      title='HomeSense Keypad 485 配电间及灯光窗帘通道清单 - rev61 场景与按键同步',author='HomeSense')
doc.build(story,onFirstPage=page_chrome,onLaterPages=page_chrome)
reader=PdfReader(temporary)
texts=[p.extract_text()for p in reader.pages]
assert all(t and len(t)>90 for t in texts)
combined='\n'.join(texts)
for s in ['rev61','起居室','0x4F','0x50','阅读-左','左中间灯','夜灯-右','14-2','01-1','02-1','13-1','12-8']:
    assert s in combined,s
assert 'Front'not in combined
assert len(rows)==71,len(rows)
for x in retained:
    assert any(address(devices[x]) in t for t in texts)
# Visual review and text checks precede replacing the stable-name output.
report={'state':'candidate_pending_visual_review','output':str(OUT),'pages':len(reader.pages),'revision':61,
        'panels':28,'keys':168,'physical_output_channels_covered':len(covered),'physical_pair_or_single_rows':len(rows),
        'native_curtains':len(motors),'new_composed_card_acceptance':41,'logical_composed_cards':47,'retained_outputs':4,
        'device_config_sha256':hashlib.sha256((LIVE/'dining51.devices.json').read_bytes()).hexdigest(),
        'historical_pdf_preserved':str(BASE),'hardware_commands_sent':0,
        'pdf_sha256':hashlib.sha256(temporary.read_bytes()).hexdigest(),
        'evidence_files':['FIELD-ALL41-COMPOSED-ACCEPTANCE-RESULT.json','MASTER-POWER-DIMMER-MATRIX-R3.json','MODULE-SEQUENCE48-DELIVERY-STATUS.json','LIFECYCLE48-SOFTWARE-GATES.json','HISTORICAL48-RECOVERED-INPUTS.json'],
        'page_text_lengths':[len(t)for t in texts]}
(QA/'verification.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n')
(QA/'rows.json').write_text(json.dumps(rows,ensure_ascii=False,indent=2)+'\n')
print(json.dumps(report,ensure_ascii=False,indent=2))

report['automation_sha256']=hashlib.sha256((LIVE/'dining51.automation.json').read_bytes()).hexdigest()
report['hap_plan_sha256']=hashlib.sha256((LIVE/'dining51.hap-plan.json').read_bytes()).hexdigest()
report['live_snapshot_sha256']=hashlib.sha256((LIVE/'TAKEOVER51-LIVE-READONLY.json').read_bytes()).hexdigest()
report['acceptance_matrix_sha256']=hashlib.sha256((LIVE/'ACCEPTANCE-MATRIX51.json').read_bytes()).hexdigest()
report['client_acceptance']='HOME51-CLIENT-ACCEPTANCE.json'
report['evidence_files'] += ['TAKEOVER51-LIVE-READONLY.json','ACCEPTANCE-MATRIX51.json','HOME51-CLIENT-ACCEPTANCE.json','DINING51-RELEASE.json']
(QA/'verification.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n')

report['scene_config_sha256']=hashlib.sha256((CURRENT/'candidate.devices.json').read_bytes()).hexdigest()
report['scene_automation_sha256']=hashlib.sha256((CURRENT/'candidate.automation.json').read_bytes()).hexdigest()
report['stable_filename_preserved']=str(OUT)
report['current_panel_labels']={p['id']:[k['label']for k in p['keys']]for p in panels}
report['readonly_recheck']={
    'at': readonly_audit['checked_at_local'],
    'passed': True,
    'panels': readonly_audit['affected_panels'],
    'labels': readonly_audit['printed_labels_compared'],
    'bindings': readonly_audit['bindings_compared'],
    'compared_pdf_sha256': readonly_audit['pdf_sha256'],
    'audit_result_sha256': hashlib.sha256((AUDIT/'AUDIT-RESULT.json').read_bytes()).hexdigest(),
    'control_requests': 0,
    'module_queries_sent': 0,
}
report['user_reported_field_acceptance'] = field_acceptance
(QA/'verification.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n')
