// Domestic futures display names, matching the native Viz catalog. Display
// only: quantities, multipliers and money are never derived from this table.
const products: Record<string, Record<string, string>> = {
  CFFEX: { IF: '沪深300股指', IH: '上证50股指', IC: '中证500股指', IM: '中证1000股指', IO: '沪深300股指', HO: '上证50股指', MO: '中证1000股指', T: '10年期国债', TF: '5年期国债', TS: '2年期国债', TL: '30年期国债' },
  SHFE: { CU: '沪铜', AL: '沪铝', ZN: '沪锌', PB: '沪铅', NI: '沪镍', SN: '沪锡', AO: '氧化铝', AD: '铸造铝合金', AU: '沪金', AG: '沪银', RB: '螺纹钢', HC: '热轧卷板', SS: '不锈钢', WR: '线材', FU: '燃料油', BU: '石油沥青', RU: '天然橡胶', SP: '纸浆', BR: '丁二烯橡胶', OP: '胶版印刷纸' },
  INE: { SC: '原油', NR: '20号胶', LU: '低硫燃料油', BC: '国际铜', EC: '集运指数（欧线）' },
  DCE: { A: '豆一', B: '豆二', M: '豆粕', Y: '豆油', P: '棕榈油', C: '玉米', CS: '玉米淀粉', RR: '粳米', JD: '鸡蛋', LH: '生猪', I: '铁矿石', J: '焦炭', JM: '焦煤', L: '聚乙烯', V: '聚氯乙烯', PP: '聚丙烯', EG: '乙二醇', EB: '苯乙烯', PG: '液化石油气', FB: '纤维板', BB: '胶合板', LG: '原木', BZ: '纯苯' },
  CZCE: { WH: '强麦', PM: '普麦', RI: '早籼稻', LR: '晚籼稻', JR: '粳稻', CF: '棉花', CY: '棉纱', SR: '白糖', OI: '菜籽油', RO: '菜籽油', RM: '菜籽粕', RS: '油菜籽', TA: 'PTA', MA: '甲醇', ME: '甲醇', FG: '玻璃', SA: '纯碱', PF: '短纤', PX: '对二甲苯', SH: '烧碱', PR: '瓶片', ZC: '动力煤', TC: '动力煤', SF: '硅铁', SM: '锰硅', AP: '苹果', CJ: '红枣', PK: '花生' },
  GFEX: { SI: '工业硅', LC: '碳酸锂', PS: '多晶硅', PT: '铂', PD: '钯' },
}

export interface NativeContract {
  /** Chinese product name with delivery month, e.g. `螺纹钢 · 2027年01月`. */
  name: string
  /** Original exchange contract code without the venue, e.g. `rb2701`. */
  code: string
}

function shanghaiYear(nowMs: number) {
  return Number(new Intl.DateTimeFormat('en', { timeZone: 'Asia/Shanghai', year: 'numeric' }).format(nowMs))
}

/**
 * Parses a Manager market id `VENUE:code`. Returns null for anything that is
 * not a known domestic futures/option contract, so Crypto symbols are untouched.
 * CZCE's three-digit year-month code takes the decade nearest to `nowMs`.
 */
export function nativeContract(market: string, nowMs = Date.now()): NativeContract | null {
  const separator = market.indexOf(':')
  if (separator <= 0) return null
  const venue = market.slice(0, separator)
  const code = market.slice(separator + 1)
  const match = /^([a-z]+)([0-9]{3,4})(?:-?([cp])-?([0-9]+(?:\.[0-9]+)?))?$/i.exec(code)
  const product = match && products[venue]?.[match[1].toUpperCase()]
  if (!match || !product) return null
  const [, , yearMonth, option, strike] = match
  const month = Number(yearMonth.slice(-2))
  if (month < 1 || month > 12) return null
  const yearCode = Number(yearMonth.slice(0, -2))
  let year = 2000 + yearCode
  if (yearMonth.length === 3) {
    const reference = shanghaiYear(nowMs)
    const decade = Math.floor(reference / 10) * 10
    year = [decade - 10 + yearCode, decade + yearCode, decade + 10 + yearCode]
      .sort((a, b) => Math.abs(a - reference) - Math.abs(b - reference))[0]
  }
  const optionText = option ? ` · ${option.toUpperCase() === 'C' ? '看涨' : '看跌'} · 行权价 ${strike}` : ''
  return { name: `${product} · ${year}年${String(month).padStart(2, '0')}月${optionText}`, code }
}

/** Single-line label: domestic contracts become `name（code）`; others are unchanged. */
export function marketLabel(market: string, nowMs = Date.now()) {
  const contract = nativeContract(market, nowMs)
  return contract ? `${contract.name}（${contract.code}）` : market
}
