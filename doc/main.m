// anveesa-doc: extracts plain text from a PDF via PDFKit and prints it
// to stdout. Usage: anveesa-doc <file.pdf>
#import <Foundation/Foundation.h>
#import <PDFKit/PDFKit.h>

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc < 2) {
            fprintf(stderr, "usage: anveesa-doc <file.pdf>\n");
            return 2;
        }
        NSURL *url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:argv[1]]];
        PDFDocument *doc = [[PDFDocument alloc] initWithURL:url];
        if (!doc) {
            fprintf(stderr, "could not open PDF: %s\n", argv[1]);
            return 1;
        }
        if (doc.isLocked) {
            fprintf(stderr, "PDF is password-protected\n");
            return 1;
        }
        NSString *text = doc.string ?: @"";
        fputs(text.UTF8String, stdout);
    }
    return 0;
}
