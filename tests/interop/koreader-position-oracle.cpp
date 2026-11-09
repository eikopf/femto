#include "lvdocview.h"
#include "lvfntman.h"
#include "lvstsheet.h"
#include <cstdio>
#include <iostream>
#include <string>
static void print(ldomXPointer pointer) {
    auto node = pointer.getNode();
    if (node && node->isElement() && (node->getNodeName() == U"img" || node->getNodeName() == U"image")) {
        auto path = UnicodeToUtf8(pointer.toString());
        auto source = UnicodeToUtf8(node->getAttributeValue("src"));
        if (source.empty()) source = UnicodeToUtf8(node->getAttributeValue("href"));
        printf("IMAGE\t%s\t", path.c_str());
        for (int i=0; i<source.length(); i++) printf("%02x", (unsigned char)source[i]);
        printf("\n");
        return;
    }
    if (!node || !node->isText() || pointer.getOffset() < 0 || pointer.getOffset() > node->getText().length()) {
        printf("INVALID\t\n");
        return;
    }
    auto path = UnicodeToUtf8(pointer.toString());
    auto text = node->getText8();
    printf("%s\t", path.c_str());
    for (int i=0; i<text.length(); i++) printf("%02x", (unsigned char)text[i]);
    printf("\n");
}
static void dump(ldomNode *node) {
    if (node->isText()) print(ldomXPointer(node, 0));
    for (int i=0; i<node->getChildCount(); i++) dump(node->getChildNode(i));
}
static void images(ldomNode *node) {
    if (node->isElement() && (node->getNodeName() == U"img" || node->getNodeName() == U"image")) print(ldomXPointer(node, -1));
    for (int i=0; i<node->getChildCount(); i++) images(node->getChildNode(i));
}
int main(int argc, char **argv) {
    if (argc != 3 && argc != 4) return 2;
    InitFontManager(lString8());
    fontMan->RegisterFont(lString8("tests/fixtures/DejaVuSerif.ttf"));
    LVDocView view;
    lString8 css;
    if (!LVLoadStylesheetFile(Utf8ToUnicode(argv[2]), css)) return 3;
    view.setStyleSheet(css, false);
    if (!view.LoadDocument(argv[1])) return 1;
    if (argc == 4 && std::string(argv[3]) == "--resolve") {
        std::string path;
        while (std::getline(std::cin, path)) print(view.getDocument()->createXPointer(Utf8ToUnicode(path.c_str())));
    } else if (argc == 4 && std::string(argv[3]) == "--images") {
        images(view.getDocument()->getRootNode());
    } else dump(view.getDocument()->getRootNode());
    return 0;
}
