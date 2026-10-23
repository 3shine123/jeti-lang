// NPString 编码 API 的 ObjC 基准 —— 与 main.jeti 输出格式完全相同，直接 diff。
// 这是「行为对齐 ObjC」铁律的可执行凭据：任何 BOM/端序/长度语义的分歧都会
// 在 diff 里现形。
//
// 编译运行：
//   clang -fobjc-arc -framework Foundation -o /tmp/objc_baseline objc_baseline.m
//   /tmp/objc_baseline
#import <Foundation/Foundation.h>
#include <stdio.h>

static void dump(const char *label, NSData *d) {
    if (!d) { printf("%-10s nil\n", label); return; }
    printf("%-10s len=%2lu :", label, (unsigned long)[d length]);
    const unsigned char *b = [d bytes];
    for (NSUInteger i = 0; i < [d length]; i++) printf(" %02X", b[i]);
    printf("\n");
}

int main(void) {
    NSString *s = @"A\u00e9\u20ac";
    NSString *astral = @"\U0001F600";

    dump("UTF8",    [s dataUsingEncoding:NSUTF8StringEncoding]);
    dump("ASCII",   [s dataUsingEncoding:NSASCIIStringEncoding]);
    dump("Latin1",  [s dataUsingEncoding:NSISOLatin1StringEncoding]);
    dump("UTF16",   [s dataUsingEncoding:NSUTF16StringEncoding]);
    dump("UTF16BE", [s dataUsingEncoding:NSUTF16BigEndianStringEncoding]);
    dump("UTF16LE", [s dataUsingEncoding:NSUTF16LittleEndianStringEncoding]);
    dump("UTF32",   [s dataUsingEncoding:NSUTF32StringEncoding]);
    dump("UTF32BE", [s dataUsingEncoding:NSUTF32BigEndianStringEncoding]);
    dump("UTF32LE", [s dataUsingEncoding:NSUTF32LittleEndianStringEncoding]);
    dump("astral16", [astral dataUsingEncoding:NSUTF16StringEncoding]);
    dump("astral32", [astral dataUsingEncoding:NSUTF32StringEncoding]);

    printf("lengthOfBytes UTF8=%lu ASCII=%lu UTF16=%lu UTF32=%lu\n",
        (unsigned long)[s lengthOfBytesUsingEncoding:NSUTF8StringEncoding],
        (unsigned long)[s lengthOfBytesUsingEncoding:NSASCIIStringEncoding],
        (unsigned long)[s lengthOfBytesUsingEncoding:NSUTF16StringEncoding],
        (unsigned long)[s lengthOfBytesUsingEncoding:NSUTF32StringEncoding]);
    printf("canConvert ASCII=%d Latin1=%d UTF16=%d\n",
        (int)[s canBeConvertedToEncoding:NSASCIIStringEncoding],
        (int)[s canBeConvertedToEncoding:NSISOLatin1StringEncoding],
        (int)[s canBeConvertedToEncoding:NSUTF16StringEncoding]);

    NSData *u16 = [s dataUsingEncoding:NSUTF16BigEndianStringEncoding];
    NSString *back = [[NSString alloc] initWithData:u16 encoding:NSUTF16BigEndianStringEncoding];
    // 比往返后的 UTF-8 字节（不比 -length：ObjC 是 UTF-16 码元数，NPString 是
    // UTF-8 字节数，属既有的 length 语义分歧，独立问题）。
    const char *rt = [back UTF8String];
    printf("roundtrip UTF16BE :");
    for (size_t ri = 0; rt[ri] != '\0'; ri++) printf(" %02X", (unsigned char)rt[ri]);
    printf("\n");
    return 0;
}
