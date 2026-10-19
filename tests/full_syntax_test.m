// full_syntax_test.m —— full_syntax_test.jeti 的纯 Objective-C 对照版（单文件）
//
// 目的：同一份语法汇测，去掉全部 jeti 专属语法后，用真实 clang / Apple Foundation
// 编译运行，作为"语法确实只是 C/ObjC"的对照验证。
//
// 覆盖（与 .jeti 对应，节号保留原编号，jeti 专属节标注"已删"）：
//   §1 C 超集基础（1.10 的 struct 值比较是 jeti 改写 extension，已删）
//   §2 类 / 继承 / 协议 / 轻量泛型 / 容器 / 分发三件套 / variadic
//      （@namespace/@using 为 jeti 专属，已删；类名改为全局名）
//   §3 表达式全家：装箱/容器字面量 / block / @selector / 运算符
//   §4 控制流：for-in / try / synchronized / autoreleasepool / __weak
//      （switch 模式匹配 4.4–4.7、@defer 4.13–4.14、@noarc 4.15 为 jeti 专属，已删）
//   §5 nullability 全位 / 内联 asm / Foundation 走查
//      （throws 注解 5.6 声明位、async/await 5.7 与整个 5.10 为 jeti 专属，已删；
//       @try/@throw/@catch 语句位是 ObjC 原生，保留）
//   1.19 外部汇编依赖 tests/full_syntax_test.s，此处不链 .s，已删（.jeti 版覆盖）
//
// 编译（真 clang）：
//   clang -fobjc-arc -framework Foundation tests/full_syntax_test.m -o /tmp/fst && /tmp/fst
// 预期：exit=0，输出确定（NSLog 行带时间戳前缀，其余行无地址/时序依赖）。
#import <Foundation/Foundation.h>
#include <stdio.h>
#include <stdarg.h>
#include <string.h>
#include <time.h>

// ═══════════════════════════════════════════════════════════════════
// §1  C 超集基础：struct/union/enum/typedef/fnptr/设计初始化/宏/typeof
// ═══════════════════════════════════════════════════════════════════

// 宏与 #pragma mark（原位透传）
#define FS_MAX(a, b)         ((a) > (b) ? (a) : (b))
#define FS_NAME              "full-syntax"
#pragma mark - §1 C superset

// 依赖拓扑：Inner 定义在 Outer 之前声明使用（C 合法形态照抄）
struct FsInner { int a; int b; };
typedef struct FsOuterTag FsOuterAlias;          // tag 仅前向
struct FsOuterTag { struct FsInner i; int tag; };

// 函数指针字段、多维数组字段、位域；`in` 是上下文关键词，可作字段名
struct FsWidget {
    int wid;
    int (*cb)(int);            // fnptr 字段
    int cells[2][3];           // 多维数组字段
    unsigned flags : 3;        // 位域
    int in;
};
typedef int FsRow4[4];                          // typedef 数组
union FsMix { int i; float f; char bytes[4]; }; // union（tag 与 struct 区分）

// enum：typedef 带体形态
typedef enum FsMode {
    FS_MODE_OFF = 0, FS_MODE_LOW = 1, FS_MODE_HIGH = 2
} FsMode;

// __attribute__：packed / format
struct FsPacked { char c; int v; } __attribute__((packed));
static int fs_format_fn(const char *fmt, ...) __attribute__((format(printf, 1, 2)));

// struct（值 == 比较为 jeti extension，此版只有指针比较目标）
struct FsPoint { int x; int y; };
typedef struct FsPoint FsPointT;

// 顶层 __asm__ 声明：原样透传（.section 指令形态）
__asm__(".text\n");

static int fs_double_it(int v) { return v * 2; }

static int fs_format_fn(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = vprintf(fmt, ap);
    va_end(ap);
    return n;
}

static void sec1_c_superset(void) {
    printf("== §1 C superset ==\n");

    // 逗号声明符列表：指针逐声明符
    int x1 = 10, x2 = 20;
    int *pa = &x1, *pb = &x2;
    (*pa)++;
    ++*pb;
    printf("1.1 ptrs %d %d\n", *pa, *pb);

    // 逗号声明符 + 函数指针类型：T (*name)(params)
    int (*fp)(int) = fs_double_it;
    printf("1.2 fnptr %d\n", fp(21));

    // C 风格强转：struct 指针 / void* / fnptr
    struct FsWidget w;
    memset(&w, 0, sizeof(w));
    void *vp = &w;
    struct FsWidget *wp = (struct FsWidget *)vp;
    wp->wid = 7;
    printf("1.3 cast %d\n", ((struct FsWidget *)vp)->wid);

    // struct 定义 + 字段走查（多维数组、位域、`in` 字段、成员 fn-ptr 直接调用）
    w.cb = fs_double_it;
    w.cells[0][0] = 1; w.cells[0][1] = 2; w.cells[1][2] = 5;
    w.flags = 5;
    w.in = 99;
    printf("1.4 widget %d %d %d %d %d\n", w.cb(3), w.cells[1][2], (int)w.flags, w.in, wp->wid);

    // typedef 数组
    FsRow4 row = { 1, 2, 3, 4 };
    printf("1.5 row4 %d\n", row[0] + row[3]);

    // union
    union FsMix m;
    m.i = 0x40490FDB;           // 3.14159f 的位型
    printf("1.6 union %.2f\n", (double)m.f);

    // enum + switch
    FsMode mode = FS_MODE_HIGH;
    const char *ms = "?";
    switch (mode) {
        case FS_MODE_OFF:  ms = "off"; break;
        case FS_MODE_LOW:  ms = "low"; break;
        case FS_MODE_HIGH: ms = "high"; break;
    }
    printf("1.7 enum %s(%d)\n", ms, (int)mode);

    // 设计初始化器（1.10 的 struct == 是 jeti extension，此处跳过该编号）
    struct FsPoint p1 = { .x = 1, .y = 2 };                 // ① 完整指定
    struct FsPoint p2 = { .y = 5 };                         // ② 部分+零填充
    struct FsPoint p3 = { .x = 1, 7 };                      // ③ 位置+指定混合
    FsOuterAlias o = { .i.a = 3, .tag = 9 };                // ⑥ 嵌套成员路径
    struct FsPoint pts[3] = { [0].x = 4, [2].y = 6 };       // ⑤ 数组元素指定
    printf("1.8 desig %d %d %d %d %d\n", p1.x + p1.y, p2.y, p3.y, o.tag + o.i.a, pts[2].y);

    // ④ 复合字面量 + 指定（cast 后跟 {）
    struct FsPoint p4 = (struct FsPoint){ .x = 8, .y = 9 };
    printf("1.9 compound %d\n", p4.x * p4.y);

    // （1.10 已删：struct 值 == / != 是 jeti checker 改写，非 C/ObjC 语法）
    // 指针身份比较
    printf("1.11 ptreq %d\n", (void *)&p1 == (void *)&p1);

    // goto / C 标签语句
    int hits = 0, pass = 0;
    for (int i = 0; i < 3; i++) {
        if (i == 1) goto skip;
        hits++;
    skip:;
        pass++;
    }
    printf("1.12 goto %d %d\n", hits, pass);

    // __typeof__ / __alignof__ / builtin
    __typeof__(x1) same = 77;
    unsigned ali = __alignof__(int);
    int expect = __builtin_expect(x1 > 5, 1);
    int compat = __builtin_types_compatible_p(__typeof__(same), int);
    printf("1.13 typeof %d %u %d %d\n", same, ali, expect, compat);

    // __attribute__((packed)) 生效（1 + 4 = 5，对齐被压掉）
    printf("1.14 packed %zu\n", sizeof(struct FsPacked));

    // for 头逗号声明列表（一个声明、一个类型）
    int acc = 0;
    for (int i = 0, j = 6; i < j; i++, j--) { acc += i + j; }
    printf("1.15 forcomma %d\n", acc);

    // 宏 + format attribute 函数
    printf("1.16 macro %d %s\n", FS_MAX(3, FS_MAX(4, 2)), FS_NAME);
    fs_format_fn("1.17 vfn %d %d\n", 1, 2);

    // xx_t 类型（POSIX typedef 识别）
    clock_t t0 = clock();
    time_t tt = 0;
    printf("1.18 xx_t %d\n", (int)(t0 >= (clock_t)0) + (int)(tt == 0));

    // （1.19 已删：extern .s 汇编符号，本版不链 tests/full_syntax_test.s）

    // `in` 作标识符
    int in = 5;
    struct FsWidget in_w;
    in_w.in = in * 2;
    printf("1.20 in %d %d\n", in, in_w.in);

    // 1.21 类型强转表达式：浮点截断 / 符号重解释 / fnptr 强转
    double fs_cd = 3.9;
    long fs_cl = (long)fs_cd;                   // 截断取整 → 3
    unsigned fs_cu = (unsigned)(fs_cd * 10.0);  // 39 → 39
    int (*fs_cfp)(int) = (int (*)(int))fs_double_it;
    printf("1.21 castx %ld %u %d\n", fs_cl, fs_cu, fs_cfp(30));

    // 1.22 故意触发的类型强转警告用例（两边同源：看各自 clang 是否都报）
    //   变量 int → char *：-Wint-to-pointer-cast（默认开；字面量 int→指针 clang 特判不报）
    //   void * → (int)：-Wvoid-pointer-to-int-cast（默认开）
    //   int * → (int)：-Wpointer-to-int-cast（默认开）
    // 运行时安全：只判非空不 deref，输出确定
    int fs_i2p = 123;
    char *fs_badp1 = (char *)123;           // 探针证实：无警告（字面量特判）
    char *fs_badp2 = (char *)fs_i2p;        // 有警告
    void *fs_vp = (void *)&fs_i2p;
    int fs_addr = (int)(void *)fs_vp;       // 有警告（经 void * 中转）
    int fs_addr2 = (int)&fs_i2p;            // 有警告：-Wpointer-to-int-cast
    printf("1.22 castwarn %d %d %d %d\n", fs_badp1 != NULL, fs_badp2 != NULL, fs_addr != 0, fs_addr2 != 0);
}

// ═══════════════════════════════════════════════════════════════════
// §2  类 / 继承 / 协议 / 轻量泛型 / 容器 / 分发三件套 / variadic
//     （.jeti 的 @namespace/@using 全部删除，类取全局名）
// ═══════════════════════════════════════════════════════════════════

// 协议：required + optional + 父协议链（Colored < Drawable）
@protocol FsDrawable
- (void)draw;
@end

@protocol FsColored <FsDrawable>
@required
- (int)color;
@optional
- (void)decorate;
@end

// @class 前向声明：先声明、后定义、再使用
@class FsForwardUser;

// —— FsForwardUser 的真定义（对上面的 @class 兑现）——
@interface FsForwardUser : NSObject
- (int)poke;
@end

@implementation FsForwardUser
- (int)poke { return 7; }
@end

// 泛型类（ObjC 轻量泛型）
@interface FsBox<T> : NSObject {
@public
    T _value;
}
- (FsBox *)initWith:(T)v;
- (T)value;
- (void)setValue:(T)v;
@end

// 真 clang 的规则：T 可用于 @interface 的方法签名与 ivar 位，
// 但 @implementation 不能带 <T>，体内只能用 id（轻量泛型是擦除语义）
@implementation FsBox
- (FsBox *)initWith:(id)v {
    self = [super init];
    if (self) { _value = v; }
    return self;
}
- (id)value { return _value; }
- (void)setValue:(id)v { _value = v; }
@end

// 基类（.jeti 里在命名空间内）
@interface FsRenderable : NSObject
- (int)render;
@end

@implementation FsRenderable
- (int)render { return 1; }
@end

@protocol FsNode <FsDrawable>
- (int)nodeId;
@end

// 继承 + 协议一致性 + @public ivar + 属性位 nullability
@interface FsSprite : FsRenderable <FsColored> {
@public
    int _id;
}
@property (nonatomic, copy, nullable) NSString *label;
@property (nonatomic, nonnull) NSString *tag;
- (FsSprite *)initWithId:(int)i;
- (int)sum:(int)first, ...;                              // 实例方法 variadic
+ (int)classSum:(int)first, ...;                         // 类方法 variadic
@end

@implementation FsSprite
@synthesize label;
@synthesize tag;

- (FsSprite *)initWithId:(int)i {
    self = [super init];
    if (self) { _id = i; }
    return self;
}
- (void)draw { printf("2.x draw sprite\n"); }            // FsDrawable 经 FsColored 传入
- (int)color { return 0x00FF00; }
- (int)render {
    // super 调用
    return 10 + [super render];
}
- (int)sum:(int)first, ... {
    va_list ap;
    va_start(ap, first);
    int total = first;
    int v;
    while ((v = va_arg(ap, int)) != 0) { total += v; }
    va_end(ap);
    return total;
}
+ (int)classSum:(int)first, ... {
    va_list ap;
    va_start(ap, first);
    int total = first;
    for (int i = 0; i < 2; i++) { total += va_arg(ap, int); }
    va_end(ap);
    return total;
}
@end

// 轻量泛型容器特化（objectAtIndex: 返回元素类型）
@interface FsTypedStore : NSObject
- (void)run;
@end

@implementation FsTypedStore
- (void)run {
    NSMutableArray<NSString *> *arr = [NSMutableArray array];
    [arr addObject:@"one"];
    [arr addObject:@"two"];
    // typed read：下标 a[1] → objectAtIndexedSubscript:
    NSString *s0 = [arr objectAtIndex:0];
    NSString *s1 = arr[1];
    printf("2.6 typed %s %s %zu\n", [s0 UTF8String], [s1 UTF8String], [arr count]);

    // 泛型类特化：FsBox<NSMutableString *>（嵌套泛型）
    // initWith 收裸 C 串的持有者——传 @"..." 对象当 C 串读会出垃圾
    FsBox<NSMutableString *> *box = [[FsBox alloc] initWith:[NSMutableString stringWithUTF8String:"boxed"]];
    printf("2.7 box %s\n", [[box value] UTF8String]);
    [box setValue:[NSMutableString stringWithUTF8String:"reboxed"]];
    printf("2.8 box %s\n", [[box value] UTF8String]);
}
@end

static void sec2_objects(void) {
    printf("== §2 objects ==\n");

    // 2.1 继承 + super + 协议一致性（required 全实现，optional 不实现也合法）
    FsSprite *sp = [[FsSprite alloc] initWithId:42];
    printf("2.1 render %d color %d\n", [sp render], [sp color]);
    [sp draw];

    // 2.2 属性：合成 getter/setter + nullable/nonnull 属性位
    sp.label = nil;
    sp.tag = @"t";
    printf("2.2 prop %s %s\n", sp.label ? "n" : "nil", [sp.tag UTF8String]);

    // 2.3 分发三件套：isKindOfClass:（isa 链）/ respondsToSelector: / isEqual:（指针身份）
    BOOL kind = [sp isKindOfClass:[FsSprite class]];
    BOOL kindSuper = [sp isKindOfClass:[FsRenderable class]];
    BOOL resp = [sp respondsToSelector:@selector(color)];
    BOOL respMissing = [sp respondsToSelector:@selector(decorate)];
    BOOL eqSelf = [sp isEqual:sp];
    BOOL eqOther = [sp isEqual:([[FsSprite alloc] initWithId:42])];
    printf("2.3 dispatch %d %d %d %d %d %d\n", kind, kindSuper, resp, respMissing, eqSelf, eqOther);

    // 2.4 variadic 方法：单段 + 类方法
    printf("2.4 variadic %d %d\n", [sp sum:1, 2, 3, 0], [FsSprite classSum:10, 20, 0]);

    // 2.5 基类直例
    FsRenderable *r = [[FsRenderable alloc] init];
    printf("2.5 base %d\n", [r render]);

    // 2.5b @class 前向声明 + 后置定义的使用
    FsForwardUser *fu = [[FsForwardUser alloc] init];
    printf("2.5b fwd %d\n", [fu poke]);

    // 2.6/2.7/2.8 容器 + 泛型
    FsTypedStore *store = [[FsTypedStore alloc] init];
    [store run];

    // 2.9 id 万能型 + @public ivar 访问
    id asId = sp;
    FsSprite *back = (FsSprite *)asId;
    printf("2.9 id %d\n", back->_id);

    // 2.10 消息发送 nil 安全（返回 0/nil，不崩）
    FsSprite *nilsp = nil;
    printf("2.10 nilmsg %d %d\n", [nilsp render], [nilsp respondsToSelector:@selector(color)]);
}

// ═══════════════════════════════════════════════════════════════════
// §3  表达式全家：装箱字面量 / 容器字面量 / block / @selector / 运算符
// ═══════════════════════════════════════════════════════════════════

// block typedef（作参数类型 + __block 捕获目标）
typedef int (^FsIntBlock)(int, int);

static int fs_call_block(FsIntBlock b, int x, int y) { return b(x, y); }

static void sec3_expressions(void) {
    printf("== §3 expressions ==\n");
    FsSprite *sp = [[FsSprite alloc] initWithId:3];

    // 3.1 装箱字面量：@int / @double / @YES @NO / @'c' / @(expr)
    NSNumber *ni = @42;
    NSNumber *nd = @1.5;
    NSNumber *nb = @YES;
    NSNumber *nc = @'A';
    int xv = 20;
    NSNumber *ne = @(xv * 2 + 1);
    printf("3.1 box %d %.1f %d %d %d\n",
           [ni intValue], [nd doubleValue], [nb boolValue], [nc charValue], [ne intValue]);

    // 3.2 数组字面量 @[...]
    NSArray *nums = @[ @1, @2, @3 ];
    NSNumber *head = (NSNumber *)[nums objectAtIndex:0];
    printf("3.2 arr %zu %d\n", [nums count], [head intValue]);

    // 3.3 字典字面量 @{}：多对 + 空字典
    NSDictionary *d = @{ @"a": @1, @"b": @(xv), @"c": @"cee" };
    NSDictionary *empty = @{};
    NSNumber *dv = [d objectForKey:@"b"];
    NSString *dc = [d objectForKey:@"c"];
    printf("3.3 dict %zu %zu %d %s\n",
           [d count], [empty count], [dv intValue], [dc UTF8String]);

    // 3.4 可变数组全家 + 下标读写（ma[0] → objectAtIndexedSubscript:/setObject:atIndexedSubscript:）
    NSMutableArray *ma = [NSMutableArray arrayWithCapacity:4];
    NSString *z = @"z";
    [ma addObject:@"x"];
    [ma insertObject:@"y" atIndex:0];
    [ma replaceObjectAtIndex:1 withObject:z];
    BOOL has = [ma containsObject:@"z"];        // isEqual: 值语义命中
    int idx = (int)[ma indexOfObject:@"z"];
    [ma removeObjectAtIndex:0];
    ma[0] = @"w";                       // 写侧下标
    NSString *m0 = ma[0];               // 读侧下标
    printf("3.4 marr %d %d %s\n", has, idx, [m0 UTF8String]);

    // 3.5 可变字典：插入-值语义替换-删除 + allKeys
    NSMutableDictionary *md = [NSMutableDictionary dictionary];
    [md setObject:@1 forKey:@"k1"];
    [md setObject:@2 forKey:@"k2"];
    [md setObject:@9 forKey:@"k1"];     // isEqual: 值语义 → 替换而非追加
    NSArray *keys = [md allKeys];
    [md removeObjectForKey:@"k2"];
    printf("3.5 mdict %zu %zu %d\n", [keys count], [md count], [[md objectForKey:@"k1"] intValue]);

    // 3.6 block：字面量 / 返回类型推断 / __block 捕获 / 传参
    FsIntBlock mul = ^int(int a, int b) { return a * b; };
    FsIntBlock add = ^(int a, int b) { return a + b; };     // 推断 int
    __block int counter = 0;
    void (^bump)(void) = ^{ counter++; };                    // __block 捕获可变
    bump(); bump(); bump();
    printf("3.6 block %d %d %d\n", mul(6, 7), add(2, 3), counter);
    printf("3.7 blockarg %d\n", fs_call_block(mul, 5, 6));

    // 3.8 @selector：respondsToSelector 用字面量
    // poke 不在 FsSprite 上 → 0；color 在 → 1
    SEL s1 = @selector(poke);
    (void)s1;
    printf("3.8 sel %d %d\n",
           [sp respondsToSelector:@selector(poke)],
           [sp respondsToSelector:@selector(color)]);

    // 3.9 字符串全家：拼接 / 值相等 / 长度 / description
    NSString *base = [NSString stringWithUTF8String:"objcr"];
    NSString *joined = [base stringByAppendingString:@"-full"];
    BOOL eq = [joined isEqualToString:@"objcr-full"];
    NSString *desc = [joined description];
    printf("3.9 str %s %d %zu %s\n", [joined UTF8String], eq, [joined length], [desc UTF8String]);

    // 3.10 NSMutableString 追加链（构造收 C 串；appendString: 收对象）
    NSMutableString *mstr = [NSMutableString stringWithUTF8String:"a"];
    [mstr appendString:@"b"];
    [mstr appendString:@"c"];
    printf("3.10 mstr %s %zu\n", [mstr UTF8String], [mstr length]);

    // 3.11 嵌套消息发送作实参（variadic 嵌套形态）
    printf("3.11 nested %d\n", [sp sum:[sp sum:1, 2, 0], 10, 0]);

    // 3.12 复合赋值 / 位 / 移位 / 逻辑短路 / 三目 / 逗号运算符 / sizeof
    int bits = 0xF0;
    bits |= 0x0F; bits &= 0x3C; bits ^= 0xFF; bits <<= 2; bits >>= 1;
    int zero = 0;
    int shortcir = (zero != 0) && (10 / zero > 0);           // 短路防除零
    int tern = (bits > 100) ? bits : -bits;
    int comma = (zero = 3, zero * 2);
    printf("3.12 ops %d %d %d %d %zu\n", bits, shortcir, tern, comma, sizeof(struct FsPoint));

    // 3.13 前后置 ++/--：值/指针解引用
    int post = 5;
    int po = post++;
    int pr = --post;
    int *pip = &post;
    (*pip)--;
    printf("3.13 incdec %d %d %d\n", po, pr, post);

    // 3.14 Class 运行期身份：[obj class]
    Class cls = [FsSprite class];
    printf("3.14 class %d %d\n", cls == [FsSprite class], [sp class] == cls);

    // 3.15 NSLog %@ 展开（对象 → description）
    NSLog(@"3.15 nplog %@ %d", joined, 7);
}

// ═══════════════════════════════════════════════════════════════════
// §4  控制流 + 运行时块：for-in / try / synchronized / autoreleasepool / weak
//     （4.4–4.7 switch 模式、4.13–4.14 @defer、4.15 @noarc 为 jeti 专属，已删）
// ═══════════════════════════════════════════════════════════════════

// typed catch 链的两个异常类（类型不同 → isa 依次匹配）
@interface FsErrLow : NSObject
@end
@implementation FsErrLow
@end

@interface FsErrHigh : FsErrLow
@end
@implementation FsErrHigh
@end

static void sec4_control_runtime(void) {
    printf("== §4 control/runtime ==\n");

    // 4.1 for-in：NSArray 遍历
    NSArray *arr = @[ @"aa", @"bb", @"cc" ];
    int seen = 0;
    NSString *last = nil;
    for (NSString *item in arr) {
        seen++;
        last = item;
    }
    printf("4.1 forin %d %s\n", seen, [last UTF8String]);

    // 4.2 嵌套 for-in + 空数组（体不执行）
    NSArray *outer = @[ @1, @2 ];
    int pairs = 0;
    for (NSNumber *o in outer) {
        for (NSString *i in arr) { pairs++; }
    }
    NSArray *empty = @[];
    int empt = 0;
    for (NSString *i in empty) { empt++; }
    printf("4.2 nest %d %d\n", pairs, empt);

    // 4.3 while / do-while / continue
    int w = 0, dw = 0, cont = 0;
    while (w < 3) { w++; }
    do { dw++; } while (dw < 4);
    for (int i = 0; i < 6; i++) {
        if (i % 2 == 0) continue;
        cont += i;
    }
    printf("4.3 loops %d %d %d\n", w, dw, cont);

    // （4.4–4.7 已删：switch 模式匹配（Bind 臂/字面量臂/比较臂/when 守卫/多值常量）
    // 是 jeti 专属，ObjC switch 只接受整型常量——见 1.7）

    // 4.8 @try/@catch（typed 链）/ @finally 恒执行
    // typed @catch 按 isa 链匹配：父类臂接住子类实例；第一个匹配臂消费异常
    int order = 0;
    @try {
        order += 1;
        @throw [[FsErrHigh alloc] init];
        order += 100;                  // 不可达
    }
    @catch (FsErrLow *e) {
        order += 10;                   // 父类臂命中子类实例
    }
    @catch (FsErrHigh *e) {
        order += 20;                   // 不可达（第一臂已消费，防双捕）
    }
    @finally {
        order += 200;
    }
    printf("4.8 try %d\n", order);     // 1 + 10 + 200 = 211

    // 4.9 嵌套 @try：内层未捕获 → 内层 @finally 先跑 → 外层捕获
    int trace = 0;
    @try {
        @try {
            @throw [[FsErrLow alloc] init];
        }
        @finally {
            trace += 1;                // 内层 finally 先执行
        }
    }
    @catch (FsErrLow *e) {
        trace += 10;
    }
    printf("4.9 nest %d\n", trace);

    // 4.10 catch 内重抛 → 传播到外层
    int rethrown = 0;
    @try {
        @try {
            @throw [[FsErrLow alloc] init];
        }
        @catch (FsErrLow *e) {
            rethrown = 1;
            @throw e;                  // 重抛
        }
    }
    @catch (FsErrLow *e) {
        rethrown += 10;
    }
    printf("4.10 rethrow %d\n", rethrown);

    // 4.11 @synchronized（互斥块）
    FsSprite *lock = [[FsSprite alloc] initWithId:1];
    int guarded = 0;
    @synchronized (lock) {
        guarded += 5;
    }
    printf("4.11 sync %d\n", guarded);

    // 4.12 @autoreleasepool：嵌套池
    int pooled = 0;
    @autoreleasepool {
        NSString *p1 = [NSString stringWithUTF8String:"pool1"];
        @autoreleasepool {
            NSString *p2 = [NSString stringWithUTF8String:"pool2"];
            pooled += [p2 length];
        }
        pooled += [p1 length];
    }
    printf("4.12 pool %d\n", pooled);

    // （4.13–4.14 已删：@defer 是 jeti 专属；ObjC 对应惯用法是 @try/@finally）
    // （4.15 已删：@noarc 块是 jeti 专属；本文件面向 -fobjc-arc 编译）

    // 4.16/4.17 __weak：置 nil 源对象后 weak 引用清零（zeroing weak）
    // ARC 下源对象是 owned（alloc+init = +1，作用域尾注入 release → dealloc → 清零）；
    // 便利构造器返回 autoreleased 对象不会销毁，验证不了 zeroing。
    __weak NSString *weakref = nil;
    {
        NSString *strong = [[NSString alloc] initWithUTF8String:"weak-target"];
        weakref = strong;
        printf("4.16 weak %d\n", weakref != nil);
    }
    printf("4.17 weakzero %d\n", weakref == nil);
}

// ═══════════════════════════════════════════════════════════════════
// §5  nullability 全位 / 内联 asm / Foundation 走查
//     （throws 注解声明位、async/await 全部为 jeti 专属，已删；
//      @try/@throw/@catch 语句位是 ObjC 原生，保留）
// ═══════════════════════════════════════════════════════════════════

// NS_ASSUME_NONNULL 区域：未标注指针 → nonnull，nullable 显式退出
NS_ASSUME_NONNULL_BEGIN
@interface FsGuarded : NSObject {
@public
    NSString *inRegion;                 // ivar 位：区域内默认 nonnull
    NSString * _Nullable optIvar;       // ivar 位显式退出（裸 nullable 只在方法位合法）
}
- (void)take:(NSString *)s;             // 参数位：区域默认 nonnull
- (void)optOut:(nullable NSString *)s;  // 区域内显式退出
- (NSString *)produce;                  // 返回位：区域默认 nonnull
@end
NS_ASSUME_NONNULL_END

@implementation FsGuarded
- (void)take:(NSString *)s { printf("5.1 region take %s\n", [s UTF8String]); }
- (void)optOut:(NSString * _Nullable)s { printf("5.2 opt %s\n", s ? [s UTF8String] : "(nil)"); }
- (NSString *)produce { return @"made"; }
@end

// 区域外：未标注 → unspecified；只允许下划线拼写（_Nullable/_Nonnull/__nullable/__nonnull）
@interface FsAnnotated : NSObject
- (void)pre:(NSString * _Nonnull)a post:(NSString * __nonnull)b;
- (NSString * _Nullable)maybe;
- (NSString * __nullable)maybeC;
@end

@implementation FsAnnotated
- (void)pre:(NSString * _Nonnull)a post:(NSString * __nonnull)b {
    printf("5.3 anno %s %s\n", [a UTF8String], [b UTF8String]);
}
- (NSString * _Nullable)maybe { return nil; }
- (NSString * __nullable)maybeC { return @"c"; }
@end

// 顶层函数：返回位 + 参数位注解（区域内显式退出用裸拼写）
NS_ASSUME_NONNULL_BEGIN
static NSString * _Nullable fs_maybe_fn(void) { return nil; }
static void fs_take_fn(NSString *_Nonnull s) { printf("5.4 fn %s\n", [s UTF8String]); }
NS_ASSUME_NONNULL_END

// enum 参数（真实 tag 关键字）+ struct 按值传参
@interface FsModes : NSObject
- (int)mix:(FsMode)m point:(struct FsPoint)p;
@end
@implementation FsModes
- (int)mix:(FsMode)m point:(struct FsPoint)p {
    return (int)m + p.x * 10 + p.y;
}
@end

// thrower：@throw 语句 + 调用方 @try/@catch（throws 声明注解为 jeti 专属，已删）
@interface FsParser : NSObject
- (int)strictParse:(NSString *)s;
- (int)looseParse:(NSString *)s;
@end

@implementation FsParser
- (int)strictParse:(NSString *)s {
    if ([s length] > 3) {
        @throw [[FsErrLow alloc] init];
    }
    return [s length];
}
- (int)looseParse:(NSString *)s {
    if (s == nil) {
        @throw [[FsErrLow alloc] init];
    }
    return 0;
}
@end

static void sec5_misc(void) {
    printf("== §5 nullability/asm ==\n");

    // 5.1–5.2 区域默认 + 显式退出
    FsGuarded *g = [[FsGuarded alloc] init];
    [g take:@"region"];
    [g optOut:nil];
    printf("5.2b produce %s\n", [[g produce] UTF8String]);
    g->inRegion = @"ivar";
    g->optIvar = nil;
    printf("5.2c ivar %s %d\n", [g->inRegion UTF8String], g->optIvar == nil);

    // 5.3–5.4 区域外下划线拼写 + 函数位
    FsAnnotated *a = [[FsAnnotated alloc] init];
    [a pre:@"x" post:@"y"];
    printf("5.4b fns %d %s\n", [a maybe] == nil, [[a maybeC] UTF8String]);
    fs_take_fn(@"fnarg");
    printf("5.4c fnret %d\n", fs_maybe_fn() == nil);

    // 5.5 enum 参数 + struct 按值
    FsModes *mo = [[FsModes alloc] init];
    struct FsPoint pp = { 1, 2 };
    printf("5.5 mix %d\n", [mo mix:FS_MODE_LOW point:pp]);

    // 5.6 @throw + 调用方捕获
    FsParser *ps = [[FsParser alloc] init];
    int caught = 0;
    @try {
        [ps strictParse:@"toolong"];
    }
    @catch (FsErrLow *e) { caught += 1; }
    @try {
        [ps looseParse:nil];
    }
    @catch (FsErrLow *e) { caught += 10; }
    @try {
        [ps looseParse:@"ok"];                            // 不抛
        caught += 100;
    }
    @catch (FsErrLow *e) { caught += 1000; }
    printf("5.6 throw %d\n", caught);

    // （5.7 已删：async/await/blockOn 是 jeti 专属）

    // 5.8 内联 asm：basic / extended(volatile+clobber) / 命名操作数 / 读写 / asm goto
    asm("nop");
    int ao = 0, aa = 10, ab = 20;
    asm volatile("add %w0, %w1, %w2\n\tsub %w0, %w0, #1"
                 : "=r"(ao) : "r"(aa), "r"(ab) : "cc");
    int sh = 0, v = 7;
    asm("mov %w[res], %w[val]\n\tlsl %w[res], %w[res], #2"
        : [res] "=r"(sh) : [val] "r"(v) : "cc");
    int rw = 3;
    asm("lsl %w0, %w0, #2" : "+r"(rw));
    int ag = 0;
    asm goto("cmp %w0, #0\n\tb.eq %l[z]" : : "r"(ag) : "cc" : z);
    goto after;
z:
    printf("5.8 asmgoto hit\n");
after:
    printf("5.8 asm %d %d %d\n", ao, sh, rw);

    // 5.9 Foundation 走查：NSNumber 全家 / NSString copy / NSArray copy
    NSNumber *n1 = [NSNumber numberWithInt:7];
    NSNumber *n2 = @7;                                     // 装箱 → 同工厂
    printf("5.9 num %lld %.2f %d %d\n",
           [n1 longLongValue], [[NSNumber numberWithDouble:2.5] doubleValue],
           [n1 isEqualToNumber:n2], [@YES boolValue]);

    NSString *orig = [NSString stringWithUTF8String:"cp"];
    NSString *cp = [orig copy];
    printf("5.9b strcopy %s %d\n", [cp UTF8String], [orig isEqual:cp] == NO);

    NSArray *src = @[ @"p", @"q" ];
    NSArray *arrcp = [src copy];
    printf("5.9c arrcopy %zu %s\n", [arrcp count], [[arrcp objectAtIndex:1] UTF8String]);
}

// （§5.10 已整节删：async × throws × 消息内 await 是 jeti 专属）

// ─── main ─────────────────────────────────────────────────────────
int main(void) {
    sec1_c_superset();
    sec2_objects();
    sec3_expressions();
    sec4_control_runtime();
    sec5_misc();
    printf("ALL SECTIONS DONE\n");
    return 0;
}
